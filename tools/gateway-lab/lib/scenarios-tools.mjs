import assert from "node:assert/strict";
import { MARKER, TOOL_MARKER, TOOL_NAME, TOOL_SCHEMA, promptDigest } from "./common.mjs";
import { clientPath, inferenceHeaders, request, summarizeHit } from "./dashboard.mjs";
import { protocolSlotsOf } from "./scenarios-routing.mjs";

function toolPrompt() {
  return `${MARKER} ${TOOL_MARKER}`;
}

function toolInput(client, model, stream) {
  if (client === "chat") {
    return {
      model,
      stream,
      max_tokens: 64,
      messages: [{ role: "user", content: toolPrompt() }],
      tools: [{ type: "function", function: { name: TOOL_NAME, description: "Echo a value", parameters: TOOL_SCHEMA } }],
    };
  }
  if (client === "responses") {
    return {
      model,
      stream,
      store: false,
      max_output_tokens: 64,
      input: [{ role: "user", content: [{ type: "input_text", text: toolPrompt() }] }],
      tools: [{ type: "function", name: TOOL_NAME, description: "Echo a value", parameters: TOOL_SCHEMA }],
    };
  }
  return {
    model,
    stream,
    max_tokens: 64,
    messages: [{ role: "user", content: [{ type: "text", text: toolPrompt() }] }],
    tools: [{ name: TOOL_NAME, description: "Echo a value", input_schema: TOOL_SCHEMA }],
  };
}

function parseSseObjects(body) {
  return body
    .split(/\r?\n/)
    .filter((line) => line.startsWith("data: "))
    .map((line) => line.slice(6))
    .filter((line) => line && line !== "[DONE]")
    .map((line) => JSON.parse(line));
}

export function parseClientToolCall(client, stream, body) {
  if (stream) {
    const objects = parseSseObjects(body);
    if (client === "chat") {
      const acc = new Map();
      for (const item of objects) {
        for (const call of item.choices?.[0]?.delta?.tool_calls || []) {
          const index = call.index ?? 0;
          const current = acc.get(index) || { id: call.id, name: call.function?.name, arguments: "" };
          if (call.id) current.id = call.id;
          if (call.function?.name) current.name = call.function.name;
          if (typeof call.function?.arguments === "string") current.arguments += call.function.arguments;
          acc.set(index, current);
        }
      }
      const first = [...acc.values()][0];
      if (!first) throw new Error("chat SSE missing fragmented tool_calls");
      return first;
    }
    if (client === "responses") {
      const done = objects.find((item) => item.type === "response.function_call_arguments.done" || item.item?.type === "function_call");
      const item = objects.find((row) => row.item?.type === "function_call")?.item;
      const args = done?.arguments || item?.arguments;
      const name = item?.name || TOOL_NAME;
      const id = item?.call_id || item?.id;
      if (!id) throw new Error("responses SSE missing function_call");
      return { id, name, arguments: args };
    }
    const tool = objects.find((item) => item.content_block?.type === "tool_use")?.content_block;
    if (!tool) throw new Error("messages SSE missing tool_use");
    return { id: tool.id, name: tool.name, arguments: JSON.stringify(tool.input || {}) };
  }
  const parsed = JSON.parse(body);
  if (client === "chat") {
    const call = parsed.choices?.[0]?.message?.tool_calls?.[0];
    if (!call) throw new Error("chat JSON missing tool_calls");
    return { id: call.id, name: call.function?.name, arguments: call.function?.arguments };
  }
  if (client === "responses") {
    const call = (parsed.output || []).find((item) => item.type === "function_call");
    if (!call) throw new Error("responses JSON missing function_call");
    return { id: call.call_id || call.id, name: call.name, arguments: call.arguments };
  }
  const block = (parsed.content || []).find((part) => part.type === "tool_use");
  if (!block) throw new Error("messages JSON missing tool_use");
  return { id: block.id, name: block.name, arguments: JSON.stringify(block.input || {}) };
}

export function toolResultInput(client, model, call) {
  if (client === "chat") {
    return {
      model,
      stream: false,
      max_tokens: 64,
      messages: [
        { role: "user", content: toolPrompt() },
        {
          role: "assistant",
          content: null,
          tool_calls: [{ id: call.id, type: "function", function: { name: call.name, arguments: call.arguments } }],
        },
        { role: "tool", tool_call_id: call.id, content: JSON.stringify({ value: "ping" }) },
      ],
      tools: [{ type: "function", function: { name: TOOL_NAME, parameters: TOOL_SCHEMA } }],
    };
  }
  if (client === "responses") {
    return {
      model,
      stream: false,
      store: false,
      max_output_tokens: 64,
      input: [
        { role: "user", content: [{ type: "input_text", text: toolPrompt() }] },
        { type: "function_call", call_id: call.id, name: call.name, arguments: call.arguments },
        { type: "function_call_output", call_id: call.id, output: JSON.stringify({ value: "ping" }) },
      ],
      tools: [{ type: "function", name: TOOL_NAME, parameters: TOOL_SCHEMA }],
    };
  }
  return {
    model,
    stream: false,
    max_tokens: 64,
    messages: [
      { role: "user", content: [{ type: "text", text: toolPrompt() }] },
      {
        role: "assistant",
        content: [{ type: "tool_use", id: call.id, name: call.name, input: JSON.parse(call.arguments || "{\"value\":\"ping\"}") }],
      },
      {
        role: "user",
        content: [{ type: "tool_result", tool_use_id: call.id, content: JSON.stringify({ value: "ping" }) }],
      },
    ],
    tools: [{ name: TOOL_NAME, input_schema: TOOL_SCHEMA }],
  };
}

async function runOneTool(runtime, collector, { client, target, stream }) {
  const { lab, gatewayBase, gatewayKey } = runtime;
  const mode = stream ? "SSE" : "JSON";
  const label = `tool ${client} -> ${target.slot} ${mode}`;
  const mark = lab.snapshot().length;
  try {
    const first = await request(
      gatewayBase,
      clientPath(client, target.publicModel, stream),
      "POST",
      toolInput(client, target.publicModel, stream),
      inferenceHeaders(client, gatewayKey),
    );
    const firstBody = await first.text();
    assert.equal(first.status, 200, `${label} declare: ${firstBody.slice(0, 400)}`);
    const call = parseClientToolCall(client, stream, firstBody);
    assert.equal(call.name, TOOL_NAME);
    if (stream && client === "chat") {
      assert.ok(call.arguments.includes("ping"), `${label}: fragmented arguments did not reassemble`);
    }
    const second = await request(
      gatewayBase,
      clientPath(client, target.publicModel, false),
      "POST",
      toolResultInput(client, target.publicModel, call),
      inferenceHeaders(client, gatewayKey),
    );
    const secondBody = await second.text();
    assert.equal(second.status, 200, `${label} result: ${secondBody.slice(0, 400)}`);
    const hits = lab.snapshot().slice(mark);
    assert.equal(hits.length, 2, `${label}: expected exactly 2 hits, got ${hits.length}: ${JSON.stringify(hits.map(summarizeHit))}`);
    assert.equal(hits[0].toolMarkerPresent, true);
    assert.equal(hits[1].hasToolResult, true);
    assert.equal(hits[0].slot, target.slot);
    assert.equal(hits[1].slot, target.slot);
    assert.equal(hits[0].toolNames.includes(TOOL_NAME), true, `${label}: first hit lost function name`);
    assert.equal(call.id, "call_lab_echo");
    assert.equal(call.name, TOOL_NAME);
    assert.equal(JSON.parse(call.arguments || "{}").value, "ping");
    if (target.slot === "chat") {
      assert.deepEqual(hits[1].roles, ["user", "assistant", "tool"]);
    } else if (target.slot === "messages") {
      assert.ok(hits[1].roles.includes("assistant"), `${label}: messages history missing assistant`);
      assert.ok(hits[1].roles.filter((role) => role === "user").length >= 2, `${label}: messages history ${hits[1].roles}`);
    } else {
      assert.ok(hits[1].roles.includes("user") && hits[1].roles.includes("tool"), `${label}: responses history ${hits[1].roles}`);
    }
    const parsed = JSON.parse(secondBody);
    const finalText =
      parsed.choices?.[0]?.message?.content ||
      (parsed.output || []).flatMap((item) => item.content || []).filter((part) => part.type === "output_text").map((part) => part.text).join("") ||
      (parsed.content || []).filter((part) => part.type === "text").map((part) => part.text).join("");
    assert.equal(finalText, target.ok, `${label}: unexpected final output ${finalText}`);
    const resultPayload = JSON.stringify({ value: "ping" });
    assert.ok(hits[1].toolCallIds?.includes(call.id), `${label}: second hit lost call id ${call.id}`);
    assert.equal(hits[1].resultDigest, promptDigest(resultPayload), `${label}: tool result content did not survive conversion`);
    if (hits[1].argumentDigest) {
      assert.equal(hits[1].argumentDigest, promptDigest(call.arguments), `${label}: tool arguments digest mismatch`);
    }
    let scenarioId = `gw.tool.${client}.${stream ? "sse" : "json"}`;
    if (client === "chat" && target.slot === "messages") scenarioId = "gw.tool.chat.messages";
    collector.pass(label, {
      status: 200,
      expectedHits: 2,
      actualHits: hits.map(summarizeHit),
      toolName: call.name,
      callId: call.id,
      reconstructedArguments: call.arguments,
      argumentDigest: hits[1].argumentDigest,
      resultDigest: hits[1].resultDigest,
      scenarioId,
      evidenceKind: "gateway_black_box",
    });
  } catch (error) {
    collector.fail(label, error, { hits: lab.snapshot().slice(mark).map(summarizeHit) });
  }
}

export async function runToolHistoryScenarios(runtime, collector) {
  const protocolSlots = protocolSlotsOf(runtime.started);
  const chat = protocolSlots.find((slot) => slot.slot === "chat");
  const responses = protocolSlots.find((slot) => slot.slot === "responses");
  const messages = protocolSlots.find((slot) => slot.slot === "messages");
  await runOneTool(runtime, collector, { client: "chat", target: chat, stream: false });
  await runOneTool(runtime, collector, { client: "chat", target: chat, stream: true });
  await runOneTool(runtime, collector, { client: "responses", target: responses, stream: false });
  await runOneTool(runtime, collector, { client: "messages", target: messages, stream: false });
  await runOneTool(runtime, collector, { client: "chat", target: messages, stream: false });
}

/* Experimental CPA v8 C ABI probe; synthetic credentials only.
 * Uses cJSON v1.7.19 (MIT), downloaded beside the build output.
 * This tests extension boundaries, not a production quota store. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "cJSON.h"

typedef struct { void *ptr; size_t len; } buffer;
typedef struct { uint32_t version; void *context; void *call; void *free; } host_api;
typedef struct { uint32_t version; int (*call)(const char *,const uint8_t *,size_t,buffer *); void (*free)(void *,size_t); void (*shutdown)(void); } plugin_api;
static SRWLOCK lock = SRWLOCK_INIT;
static cJSON *attempts;
static const char *str(cJSON *o,const char *k) { cJSON *v=cJSON_GetObjectItemCaseSensitive(o,k); return cJSON_IsString(v)?v->valuestring:""; }
static double num(cJSON *o,const char *k) { cJSON *v=cJSON_GetObjectItemCaseSensitive(o,k); return cJSON_IsNumber(v)?v->valuedouble:0; }
static int flag(cJSON *o,const char *k) { return cJSON_IsTrue(cJSON_GetObjectItemCaseSensitive(o,k)); }
static double now_ms(void) { FILETIME f; ULARGE_INTEGER u; GetSystemTimeAsFileTime(&f); u.LowPart=f.dwLowDateTime;u.HighPart=f.dwHighDateTime;return (double)(u.QuadPart/10000-11644473600000ULL); }
static cJSON *read_json(const char *p) {
    FILE *f=fopen(p,"rb"); if(!f)return cJSON_CreateObject();
    fseek(f,0,SEEK_END);long n=ftell(f);rewind(f);if(n<0||n>1024*1024){fclose(f);return cJSON_CreateObject();}
    char *s=calloc((size_t)n+1,1); if(!s){fclose(f);return cJSON_CreateObject();}fread(s,1,n,f);fclose(f);
    cJSON *o=cJSON_Parse(s);free(s);return o?o:cJSON_CreateObject();
}
static int save_json(const char *p,cJSON *o) {
    char *s=cJSON_PrintUnformatted(o);if(!s)return 0;
    FILE *f=fopen("probe-state.tmp","wb");int ok=f&&fwrite(s,1,strlen(s),f)==strlen(s);
    if(f&&fclose(f))ok=0;free(s);
    return ok&&MoveFileExA("probe-state.tmp",p,MOVEFILE_REPLACE_EXISTING|MOVEFILE_WRITE_THROUGH);
}
static cJSON *ok(cJSON *result) { cJSON *o=cJSON_CreateObject();cJSON_AddBoolToObject(o,"ok",1);cJSON_AddItemToObject(o,"result",result);return o; }
static cJSON *error(const char *message) {
    cJSON *o=cJSON_CreateObject(),*e=cJSON_CreateObject();cJSON_AddBoolToObject(o,"ok",0);
    cJSON_AddStringToObject(e,"code","probe_error");cJSON_AddStringToObject(e,"message",message);cJSON_AddNumberToObject(e,"http_status",503);cJSON_AddItemToObject(o,"error",e);return o;
}
static void log_call(const char *method,cJSON *request,cJSON *response) {
    cJSON *o=cJSON_CreateObject();cJSON_AddNumberToObject(o,"atMs",now_ms());cJSON_AddStringToObject(o,"method",method);
    cJSON_AddItemToObject(o,"request",cJSON_Duplicate(request,1));cJSON_AddItemToObject(o,"response",cJSON_Duplicate(response,1));
    char *s=cJSON_PrintUnformatted(o);FILE *f=fopen("probe-events.jsonl","ab");if(f&&s){fputs(s,f);fputc('\n',f);}if(f)fclose(f);free(s);cJSON_Delete(o);
}
static double reset_ms(const char *message) {
    const char *p=strstr(message,"Your limit resets at ");if(!p)return 0;p+=strlen("Your limit resets at ");
    int y,m,d,h,mi,s,ms;SYSTEMTIME t={0};FILETIME f;ULARGE_INTEGER u;
    if(sscanf(p,"%d-%d-%dT%d:%d:%d.%dZ",&y,&m,&d,&h,&mi,&s,&ms)!=7)return 0;
    t.wYear=(WORD)y;t.wMonth=(WORD)m;t.wDay=(WORD)d;t.wHour=(WORD)h;t.wMinute=(WORD)mi;t.wSecond=(WORD)s;t.wMilliseconds=(WORD)ms;
    if(!SystemTimeToFileTime(&t,&f))return 0;u.LowPart=f.dwLowDateTime;u.HighPart=f.dwHighDateTime;
    return (double)(u.QuadPart/10000-11644473600000ULL);
}
static void observe_usage(cJSON *req) {
    cJSON *failure=cJSON_GetObjectItemCaseSensitive(req,"Failure");
    if(!flag(req,"Failed")||num(failure,"StatusCode")!=429)return;
    cJSON *body=cJSON_Parse(str(failure,"Body")),*e=cJSON_GetObjectItemCaseSensitive(body,"error");
    const char *message=str(e,"message");double until=reset_ms(message);
    int known=strstr(message,"You've reached your 5-hour usage limit for your plan.")||strstr(message,"You've reached your weekly usage limit for your plan.")||strstr(message,"You've reached your monthly usage limit for your plan.");
    if(known&&until>now_ms()&&until<now_ms()+40.0*86400000&&strcmp(str(e,"code"),"RATE_LIMITED")==0&&strcmp(str(e,"type"),"rate_limit_error")==0&&*str(req,"AuthID")) {
        cJSON *state=read_json("probe-state.json");const char *id=str(req,"AuthID");
        if(until>num(state,id)){cJSON_DeleteItemFromObjectCaseSensitive(state,id);cJSON_AddNumberToObject(state,id,until);save_json("probe-state.json",state);}cJSON_Delete(state);
    }
    cJSON_Delete(body);
}
static cJSON *pick(cJSON *req,cJSON *control) {
    cJSON *r=cJSON_CreateObject();
    if(flag(control,"schedulerError")){cJSON_Delete(r);return error("deliberate scheduler error");}
    if(flag(control,"invalidScheduler")){cJSON_AddStringToObject(r,"AuthID","absent-auth");cJSON_AddBoolToObject(r,"Handled",1);return ok(r);}
    if(flag(control,"rejectAll")){cJSON_AddBoolToObject(r,"Handled",1);cJSON_AddBoolToObject(r,"Reject",1);cJSON_AddStringToObject(r,"RejectReason","probe rejects all candidates");return ok(r);}
    if(!flag(control,"deadlineScheduling")){cJSON_AddBoolToObject(r,"Handled",0);return ok(r);}
    cJSON *state=read_json("probe-state.json"),*candidates=cJSON_GetObjectItemCaseSensitive(req,"Candidates"),*candidate,*selected=NULL;
    cJSON_ArrayForEach(candidate,candidates) {
        if(num(state,str(candidate,"ID"))>now_ms())continue;
        if(!selected||num(candidate,"Priority")>num(selected,"Priority"))selected=candidate;
    }
    cJSON_AddBoolToObject(r,"Handled",1);
    if(selected)cJSON_AddStringToObject(r,"AuthID",str(selected,"ID"));else {cJSON_AddBoolToObject(r,"Reject",1);cJSON_AddStringToObject(r,"RejectReason","all probe credentials blocked");}
    cJSON_Delete(state);return ok(r);
}
static cJSON *after_auth(cJSON *req,cJSON *control) {
    if(flag(control,"interceptorError"))return error("deliberate interceptor error");
    const char *id=str(req,"RequestID");double count=num(attempts,id);
    cJSON_DeleteItemFromObjectCaseSensitive(attempts,id);cJSON_AddNumberToObject(attempts,id,count+1);
    cJSON *r=cJSON_CreateObject();
    if(flag(control,"stopSecondAttempt")&&count>=1) {
        cJSON_AddBoolToObject(r,"Terminate",1);cJSON_AddNumberToObject(r,"StatusCode",503);
        cJSON_AddStringToObject(r,"ResponseBody","eyJlcnJvciI6eyJjb2RlIjoicHJvYmVfbm9fcmVwbGF5In19");
    }
    return ok(r);
}
static cJSON *handle(const char *method,cJSON *req,cJSON *control) {
    if(!strcmp(method,"plugin.register")||!strcmp(method,"plugin.reconfigure")) {
        return cJSON_Parse("{\"ok\":true,\"result\":{\"schema_version\":6,\"metadata\":{\"Name\":\"ocg-probe\",\"Version\":\"0.1.0\",\"Author\":\"OCG experiment\",\"GitHubRepository\":\"https://github.com/router-for-me/CLIProxyAPI\"},\"capabilities\":{\"scheduler\":true,\"scheduler_across_priorities\":true,\"request_interceptor\":true,\"request_lifecycle_plugin\":true,\"response_interceptor\":true,\"stream_chunk_interceptor\":true,\"usage_plugin\":true}}}");
    }
    if(!strcmp(method,"scheduler.pick"))return pick(req,control);
    if(!strcmp(method,"request.intercept_after"))return after_auth(req,control);
    if(!strcmp(method,"usage.handle")) { if(flag(control,"learnDeadline"))observe_usage(req);return ok(cJSON_CreateObject()); }
    if(!strcmp(method,"request.complete")) { cJSON_DeleteItemFromObjectCaseSensitive(attempts,str(req,"RequestID"));return ok(cJSON_CreateObject()); }
    if(!strcmp(method,"request.intercept_before")||!strcmp(method,"response.intercept_after")||!strcmp(method,"response.intercept_stream_chunk"))return ok(cJSON_CreateObject());
    return ok(cJSON_CreateObject());
}
static int call(const char *method,const uint8_t *request,size_t length,buffer *response) {
    if(!response||!method)return 1;response->ptr=NULL;response->len=0;
    cJSON *req=cJSON_ParseWithLength((const char*)request,length);if(!req)req=cJSON_CreateObject();
    // Delay before locking lets scheduling proceed while an observer is pending.
    if(!strcmp(method,"usage.handle")) {
        AcquireSRWLockExclusive(&lock);cJSON *control=read_json("probe-control.json");DWORD delay=(DWORD)num(control,"usageDelayMs");cJSON_Delete(control);ReleaseSRWLockExclusive(&lock);
        if(delay)Sleep(delay);
    }
    AcquireSRWLockExclusive(&lock);
    cJSON *control=read_json("probe-control.json"),*reply=handle(method,req,control);log_call(method,req,reply);
    char *raw=cJSON_PrintUnformatted(reply);if(raw){response->ptr=raw;response->len=strlen(raw);}
    cJSON_Delete(reply);cJSON_Delete(control);cJSON_Delete(req);ReleaseSRWLockExclusive(&lock);return raw?0:1;
}
static void release(void *p,size_t length) { (void)length;free(p); }
static void shutdown(void) { /* CPA owns process lifetime; no global files touched. */ }
__declspec(dllexport) int cliproxy_plugin_init(const host_api *host,plugin_api *plugin) {
    if(!host||host->version!=1||!plugin)return 1;attempts=cJSON_CreateObject();
    plugin->version=1;plugin->call=call;plugin->free=release;plugin->shutdown=shutdown;return 0;
}

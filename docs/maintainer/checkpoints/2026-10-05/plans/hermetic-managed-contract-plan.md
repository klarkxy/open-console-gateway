# Hermetic built-in and managed verification candidate plan

Source-supported candidate from independent planner hermetic_builtin_contract_plan; primary transcribed. NOT IMPLEMENTED/ACCEPTED. No source/Cargo/runtime/helper/credential/Git/official network operation. Full sole-owned-CPA CLI outcome remains; retry/scheduling/deadline/noReplay behavior assertions remain unresolved pending real CPA sends.

## Current proof

- managed_key_verify43-90 retains debug generation-keyed target map/drop guard but explicitly no longer uses URL for production hop; execute calls send_validated_generation around424.
- cpa_test_endpoints28-68 has compilefeature cutoff and boundedpureparser: explicit nonzero-port HTTP loopback, duplicate/unknown/userinfo/query/fragment rejection, preservecanonicalpath/query. Feature-off ignoresmapping.
- cpa_projection/endpoints349-382 and cpa_execution/identity1775-1800/1891-1904 rewriteGo/Goat coherently; Zenmissing both. Projection/live origin/fingerprint/grants mustagree.
- managedstage299-338 persistsPending andbumpsrevision; completion495-613 fencespoststage revision/generation/credential/version/binding, commitsandbumpsagain. db/usage_store437-490 advancesmaterialversionsduringstage, notagaincompletionwithsamecipher.
- accepted validation-fitness/review19/56 requirespersistbeforehop andnarrowpendinggrant/poststagefence. OldsameCASconcurrent test1266-1315 expects2sends/+1; newentryCASserializesfirststageinvalidatingsecondbeforehop.
- protocol_probe499-503 NotSent disabledbinding mappedallNotSent503 in account_model_test56-57; existing integration244-285 requires200 successfalse httpStatusnull unchangedCAS/zerosend. This is a bounded preservedcontract option, not permissiontoreturn200forallinfrastructurefailures.
- No namedEnvGuard established; actualmanagedinstaller/guardsabove. Cscript219-228 alreadychild-scopesenv. No globalenvmutation underparallelRusttests.

## Zen legitimate keyless trace

domain/provider487-500 reservedCredentialKindNone singleton; destination714-746 mapsZen+None. Projection endpoints297-303 noneauth, build166-203/209-234 missingmaterialallowedallNoneroutes/auth_idNone/OpenCodeSession. yaml211-233/299-308 blankkey/no-material; frozenGo projection377-395 blankallowedonlyidentifiedcredential/nonemptyexclusivelyNoneroutes; host730-752/870-884 registersnonOAuthauth/reportsepoch. Identity634-681 matchingno-material/authId initialepoch0, adopt_stamp_for724-737 allowsHttpNone only, missingZen; validated2430-2478 andpolicy321-325 compareagainstactualnonzeroappliedstamp. Explanationauthority314-373/480-483/685 independentlyusestamproute andcanlookeligiblewithoutprovingadmission. Source-supportedgapcandidate, no runtimeclaim; NativeCPA/OAuthPending/Absent remainseparateauthority.

## Proposed proportional contract (root selection/fitness still required)

1. Reuseexistingfeature+pureparser, immutableinstance-scoped fixturemap associatedwithCoreState installedcompletebeforefirstownedapply. Hiddenfeature-onlyinstaller; secondinstallationfails. No serialization/backup/HTTP/processenv/threadlocal/tasklocal/generationregistry/transportonlyoverride/newservice. Sharemapexplicitlyacrossprojectionendpoint/sealedgrant, liveface, explanationauthority andvalidatedlookup. Participatingbuiltinmissingrequiredmapfailsbeforeofficialfallback. Preservechild-scopedCLIenv path; feature-offineffectiveinstaller/officialURLs. RequiredparticipantsGo/Goat/Zen only; Zenkeyopencode-zen-free mustnotprefix-matchGo. Rewriteonlyscheme/host/port; Go physicalpath/zen/go/v1/chat/completions, Zen/zen/v1/chat/completions, Goatcanonicalprotocolpath; oldrelativepath assertions complementedbyexactphysicalprefix.

2. Zenadoptionexactauthoritativebuiltinsingleton+None+emptymaterial+noNativeOAuthassociation withexistinguniqueCID/version/binding/auth/materialstamp. HTTPNone unchanged. No genericemptycipher/destNone grant; counterfeitprovider/singleton, keyedmissingmaterial, NativePending/Absent, ambiguous/stalebindings unavailable; currentgrants/setup/scope/enablement stillgate.

3. ManagedphaseCAS: observeR0originalrow/entryCAS; heldfakelistenerRstage exactly+1 encryptedcandidatePendingDisabled/currentstagedversion/binding/material, noReady; unchangedcompletionRdone exactlyadditional+1, successReadyEnabled, failurePending, norestagerotation. Invalid/stale/ineligible entry0writes0sends. Afterstagemutationobserveownreceipt, latecompletionconflicts/noextraPromotion. RetiredV2tombstonewhileheldpreservesRstage/stagedrow thenreleaseRdone. SharedR0requests one stage+one physicalsend+loserprehopconflict+onecompletion; separatefreshpoststageCAS replacement A/B sequence preserves2sendstalecompletioncoverage (Aheld, Bstagefresh, Alatecannotwrite, Bcompletionownreceipt). Compareciphers/versions/bindings/allrevisions, norestart/reapplyduringheldwindow. No blanket+2 replacement.

4. Preserve disabledbinding observational modeltest200/successfalse/httpStatusnull/redactedreason/duration/zerosend/nosibling/unchangedstate. Prefertypedpreflightrefusal, notstringmatching. Othermissing/unreadyCPA/malformedauthority/bridgeunavailable remainserviceerrors. Rootmustselectthisnarrowoption.

## Primary selected contract and finite fitness corrections

The primary selects the instance-local feature-only map and the narrow typed disabled-binding refusal described above. This is an implementation contract, not acceptance. Read hermetic-managed-fitness.md with this plan; both finite corrections are mandatory.

- Install existing UsageSyncRuntime non-network fetch seam at harness state construction before gateway startup, covering scheduled/manual Go usage fetches. Disable reactive refresh through its existing instance-local test setter only in tests where reactive usage is outside the assertions. Tests specifically asserting usage/reactive behavior retain controlled local fetches and scoped facts. Keep external-deny isolation and measure usage separately; do not fabricate trusted quota or widen unrelated product usage APIs.
- Install a complete immutable map before first render, not merely before first successful apply. Explicitly seal absence at first render. Use existing state synchronization to reject duplicate, late and racing installation; no process-environment fallback when this instance map is present. Adapt successive-origin outcome loops to fresh states or one stable listener; dead-origin/proxy/timeout cases get their own complete mapped state. No map reinstall or restart/reapply during held stale-completion windows.
- Remove the obsolete managed generation-keyed no-op override/guard after caller migration. Preserve the CLI child-scoped environment lane and feature-off official URL behavior. Feature-off positive tests must not send to official providers.
- For disabled binding only, carry a typed preflight refusal to the account model-test response, preserving 200/success=false/httpStatus=null/no-send/unchanged revision. Do not infer this condition from reason strings or broaden infrastructure refusal handling.

## Coherent ownership after active29742 handoff

- fixtureparser+statehiddeninstaller/siblingtests.
- cpa_projection types/build/endpoints/tests; cpa_execution/project/callback/identity/explain +identity/authority andallsignatureconsumers/siblingtests.
- exactZenpredicate/tests.
- managed/account verify/provider probes integrationfixtures+ownedhelper, mapbeforeadoption/measuredsnapshot; preservemodel/nonstream/budget/body/key/protocol/path/session/physicalcounts.
- account_model_test/protocol_probe typedrefusal+sibling/integrationtests; existingmanaged/DB2phasebehaviorremainsunlessactualintegritydefect.
- pairedEN/ZHcontractdocs savedpending/stagecompletion/stalefence/savedversusapplied, noclaimedacceptance.

## Required actual acceptance and recovery

Feature-offpureparser/projectionretainofficial/ignoremapping; no positivebuiltin networktests. Feature-onparallel independentharnesses distinctloopbackmarkers/maps, eachonlyownlistener/no globalenvserializationworkaround. Projection/applied/livegrant/fingerprint/physicalURLagree. Managedexactbody/auth/session/path andzeroalternativeaccount/protocols. Malformed/missingmapbeforephysicalsend; setup/paniccleanupnochild/listenerbeforeprofiledelete. Zenrealregisterednoneepochpositive plusHTTPNone/native/counterfeit/ambiguousnegatives. Managedsuccess/authvalid429/failure/redirect/blacklist/closedport/timeout/stale/concurrent phases independentreceipts. Ordinary429temporary/nodurablequota withoutauthoritativefacts. Preserve429→403→200/sticky/408unknownnoReplay/sameaccountconnectretry/sharedabsolutedeadline untilactualCPAtrace/run; no OCG outerretryauthorization.

SoleCargoownercompilesbothvariants/allaffectedconsumers, runfinitehermetictargets beforeexpansion. Recoveryrestorescoherentownedsource/tests/stopsownfixture; no productmigration. Pendingencryptedcandidate intentional; failed/stalecompletionneverrestoresoldmaterial/undoesconcurrentreplacement. Rootselectsinstaller/APIoption/fitness, actualruntime/wholeCLI remainsopen. No newdependency/publication.

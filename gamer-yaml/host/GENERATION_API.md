# Automation generation and diagnostic contract

All public management calls use `POST /api/extensions/gamer-yaml/call` with
`{ "action": "…", "values": { … } }`. The extension must be running. These
management actions are deliberately unavailable to plugin/model callers.
Generation and diagnostics never acquire a device control lease.

## Candidates

- `generation.readiness {}` returns `{ready, reason, model}`. This checks the optional AI plugin's live lifecycle/dependency contract and the current configuration's successful model/image probe. Tool-calling failures do not block image-to-JSON generation
- `generation.create` creates a manual candidate without AI. `generation.start` creates and starts a bounded model/validation loop. Both accept `{package_id, name, goal, samples, yaml?, templates?, args?, limits?}`. Names are relative automation YAML paths. `samples` should contain `{sample_id, plugin_id: "gamer-video" | "gamer-yaml"}`; each resolves only to the selected package's `samples/<id>.gamersample`. Small inline `{manifest, files:[{path,base64}]}` bundles are accepted for fixtures and still undergo canonical validation
- `generation.list {package_id}` returns `{candidates}`. `generation.get {package_id,candidate_id}` returns `{candidate,base_yaml,base_exists,verification_scope}`. Evidence lives in an immutable private snapshot; status polling does not re-read clip data
- `generation.edit {package_id,candidate_id,expected_revision,yaml,templates?,args?}` updates a stopped draft and invalidates its report. The selected sample set and original goal cannot be changed through this action
- `generation.validate {package_id,candidate_id}` starts production offline validation of every original sample. `generation.retry` starts a new bounded correction loop using the same evidence and goal. `generation.cancel` cancels the exact current request and fences late results
- `generation.template {package_id,candidate_id,name,pending?}` returns `{name,mime_type,base64,width,height}` for one reviewed crop. `pending:true` previews a staged diagnostic proposal

Crops have `{name,sample_id,frame_id,rect:[x,y,width,height]}`. The server extracts
PNG pixels from that original frame, checks geometry and records provenance.
No model-created image bytes or arbitrary paths are accepted. Changed templates
that could affect another script/function, including ambiguous dynamic or
short-name references, require a new unique name.

Limits default to `{max_attempts:3,max_seconds:180,max_tokens:40000}` and are
always finite. A service-wide limit admits at most two generation/validation
jobs. Model output tokens and request duration are bounded; reported usage is
accumulated and input allowance is estimated conservatively before each call.
This is a token/request budget, not a currency guarantee. Missing usage or a
cancelled in-flight request makes cost unknown and disables automatic retry;
manual editing/validation remain usable.

All immutable media remain in offline verification. The model receives a
bounded deterministic image subset (at most 96 PNGs / 32 MiB), preserving every
sample's START/END and spreading middle frames over time. The prompt records
omissions explicitly. Clip bytes never go to the model. Real clip/anchor
preflight occurs before a paid model request.

## Review, publication and undo

`generation.save` accepts
`{package_id,candidate_id,expected_revision,expected_version,mode}`.

- `mode:"draft"` preserves private staging only
- `mode:"validated"` requires the current content fingerprint and a passed report for every originally selected sample. `expected_version` is the candidate's `base_version`; concurrent production edits cause conflict

Publication stages the complete resource directory under the shared snapshot
barrier, updates only the chosen script/crops and writes a private revision
record containing immutable validation provenance. Prepared history and a
transaction journal permit recovery before the server begins serving. No raw
candidate snapshots or private history are included in Package export.

`generation.history {package_id}` returns `{version,revisions}`.
`generation.rollback {package_id,revision_id,expected_version}` **undoes the
selected change**, restoring its touched artifacts' before-content/absence.
Other current resources remain untouched. A touched artifact changed since
that revision causes conflict. Undo itself becomes a new recorded revision.

Verification metadata includes exact tested arguments, sample fingerprints,
source/crop provenance and whether the script defaults equal the tested
arguments. Other parameters, unseen branches, actual games/devices and real
model quality are not proven by the offline report. Reusable parameterized
scripts are permitted without falsely claiming all parameter values pass.

Deleting a Package removes only that package's private candidates/snapshots
and history. Active work holds PackageActivity, so deletion cannot race it.
Disabling or uninstalling a plugin does not delete dormant package data.

## AI cooperation

The AI plugin's private provider submit/poll actions are
`automation.readiness`, `automation.generate`, `automation.result` and
`automation.cancel`, with `gamer-yaml` as the declared optional caller.
Generation requires the caller's `ai.connect` permission. Credentials remain
inside the existing AI Settings service. Submit/poll calls are short so model
network waits do not hold extension lifecycle gates.

`conversation.message` can carry a user attachment
`automation:{script_id,run_id?,candidate_id?,device_id?}`. The server binds and
persists the selected package/script/version/run context. Model arguments
cannot create or expand that scope. Runs must match the selected YAML
entrypoint and target; there is no global-latest fallback.

Tools read the selected script, scoped templates/sample frames, validation,
run summary, paginated steps and individual true Trace images. Image absence
or expiry is explicit. Temporary image bytes are excluded from persisted chat
history/request diagnostics. Automation-only analysis does not pause an
existing gameplay session or receive gameplay/memory tools.

`automation_propose` stages `candidate.pending_proposal` only. The model cannot
edit/save/rollback formal resources or claim user approval. The authenticated
user applies it through
`generation.apply_proposal {package_id,candidate_id,expected_revision,proposal_id}`;
this invalidates validation and requires another full validation and final save.

Protocol fixtures, generated pixels and loopback suppliers test mechanics.
They are explicitly not evidence of real-model generation quality or actual
Android/CDP game behavior.

Execution timing settings (default visual timeout and before/after-click delays)
are captured from the same production Settings source when the candidate is
created. They participate in the content fingerprint, replay host and revision
provenance. A changed global setting invalidates the report; publication checks
and commits under the setting writer's gate. Synthetic fixtures explicitly save
0 ms in their isolated production configuration rather than silently bypassing
production delays in the validator.

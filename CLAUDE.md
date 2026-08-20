# owlmake — the rule everything else follows from

## The purpose

**owlmake exists so that an ontology repository can delete its `Makefile` and its
ODK files and still build.** Not "build with fewer dependencies", not "build
faster" — build *at all*, from `owlmake.yaml` alone.

Any work here is unfinished while a repo still needs those files. That is the
acceptance test, and it is not met by a green test suite: delete the `Makefile`
and the `<id>-odk.yaml`, run the build, run the QC, run the repo's own targets,
and see them work.

Parity can't mean "shell out to ROBOT/dicer/dosdp-tools" — none of them will be there. It means owlmake's own commands must do everything ODK's recipes ask ROBOT to do.

## The rule

**Every piece of information the build uses must flow through the plan. A single
case where it does not is a critical bug.**

The plan (`owlmake.yaml`) is the whole contract. Ingest — `src/odk/` — is the
only thing that may read a `Makefile`, an ODK yaml, or anything else about how a
repo used to be built. It resolves what it finds and writes the *consequence*
into the plan. Everything downstream reads the plan and nothing else.

This is not a style preference. A build that consults a Makefile "just for this
one thing" works perfectly until the Makefile is deleted, and then fails — or
worse, silently does less. Both have happened here:

- the executor read `$(SRC)`, `$(ROBOT)` and `rule_for(…)` at build time, so a
  plan-only repo lost its edit file and its dependency graph;
- target dispatch asked the filesystem whether an ODK yaml existed, so which QC
  ran depended on a file the plan never mentioned;
- a bare build stopped at the release artefacts because the repo's default goal
  — `all: … release qc` — lived only in the Makefile, and the QC quietly stopped
  running under a workflow still called `ontology_qc`.
- ingest evaluated the Makefile's `ifeq ($(MIR),true)` under the flags of the run
  that happened to generate the plan, so the plan described one flag combination
  and no other. Building with a different one made owlmake declare the plan stale
  and ask for a regeneration — an instruction a plan-only repo cannot follow,
  because the build configuration it would regenerate from is exactly what was
  deleted. `TODAY` went the same way: unknown to owlmake, it substituted at
  ingest and froze one release date into the plan in 93 places.

Each looked like a small exception. Each broke the purpose.

### What this means in practice

- **Ingest resolves, the plan records, execution obeys.** If execution needs to
  know something, ask what plan field carries it. If none does, add one — do not
  reach for the repo.
- **The executor cannot reach a Makefile by construction.** `crate::build::Repo`
  holds directories and a `&Plan`. Keep it that way; if you find yourself
  wanting `OdkRepo` there, the answer is a new plan field.
- **A decision made by reading the repo belongs at plan time**, written down as
  its result — `variables`, `default_targets`, `native_qc`, `requires` are all
  this shape.
- **Nothing in a plan should be inert.** A step that never runs makes the plan
  claim work it does not do; drop it at ingest instead.
- **If a target cannot be named from the plan, it cannot be run.** Record every
  rule, including the grouping ones that have prerequisites and no recipe.

### The decision procedure

The rule says information must flow through the plan. These five say *which*
reads are violations.

**P1 — Naming.** The plan names every path the build reads or writes. Execution
may read the **contents** of a path the plan names; it may never **discover** a
path, or a set of paths.

**P2 — Graph vs data.** Anything that decides **which steps exist** is resolved
at ingest and recorded. Anything that is merely **data a step consumes** is read
at run time from a plan-named path. A useful sharpening: *if a curator can change
it as part of ordinary curation, it is data.* Adding an `owl:imports` and its
`catalog-v001.xml` line is curation — so the plan names the catalog file and
execution reads it, rather than freezing the resolved map. Adding a DOSDP pattern
to the build is not curation; that is a change to what the build does, and it is
meant to show up as a plan diff in review.

**P3 — Run inputs.** A caller's choice for *this run* — which targets, which
groups to rebuild, which release date, whether to re-fetch mirrors — is not plan
content. But the plan must **declare the parameter space**, so a repo with no
Makefile still knows the switches exist and what they cover; and a run input must
be passed to the steps that need it, never published to the process environment.

The plan must therefore describe the build under **every** value of a run input,
not the one ingest happened to see. Where a Makefile branches on a flag —
`ifeq ($(MIR),true)` — ingest resolves **both** branches and records each as
declared parameter space; freezing the branch it parsed writes a caller's choice
into the contract. The test is the acceptance test applied to flags: plan a repo,
move the Makefile and ODK yaml out of the tree, then build with a **different**
flag combination from the one that planned it, and get what ODK gives for those
flags.

So **regenerating the plan is never the answer to changing a flag.** If owlmake
ever answers "this plan is stale, regenerate it" to a caller who only changed a
run input, that is this bug: the repo it would regenerate from is precisely what
the plan exists to replace. A staleness check must compare the plan against the
build configuration under a fixed configuration, never under the caller's.

A date is a run input. The plan may carry a default version, but the version must
be read at run time from one field — never pre-expanded into every version IRI,
which fixes one release forever.

**P4 — Determinism.** Same plan plus same run inputs ⇒ same bytes. Anything else
that changes output is a bug.

**P5 — Honesty.** No silent skip. A check that cannot run **fails**. A step in
the plan **runs**. A declared file that is missing is an **error**, not a filter.

## owlmake ships nothing

A repo built with owlmake has no ROBOT, no ODK, no Java, no `dicer-cli`, no
`dosdp-tools`. The single `om` binary is all there is, and it bundles no
third-party binary: `om report` IS the reimplementation of `robot report`, and
`sed`, `grep`, `comm`, `jq` and `sssom` are owlmake's own code too.

So there is no category of "tools we have" as against "tools we must write":
**every tool an ODK recipe invokes has to exist as owlmake's own
reimplementation, or the check does not run.** In particular, interpreting a
repo's recipe rather than reimplementing its QC does not avoid a
reimplementation — a recipe reading `$(ROBOT) report …` resolves `robot` to `om`
and lands on owlmake's own report. Interpretation decides only *which* owlmake
code runs, and in what order.

The one boundary: a repo's **own** scripts (MONDO's Perl, EFO's Python) are repo
content, not ODK tooling. They run; an interpreter is an ordinary environment
dependency. owlmake names them in `requires` and otherwise leaves them alone.

## A number is only as good as what it was measured over

Parity work is measurement, and a measurement that is wrong in the safe-looking
direction is worse than no measurement: it reads as progress. Four hazards
account for every bad number this project has produced, and each one defeats the
guard for the hazard above it — so all four have to hold at once.

**Derive the comparison set; never curate it.** `om make --list-targets` prints
what the repo can build. A hand-written list of files becomes the definition of
"done", and the targets outside it are exactly where the defects sit: UBERON's
42 bridge targets sat outside its list at **0 of 42** identical, hiding five
distinct defects including a rule-language construct that never fired at all.
Do not treat a *goal* as the surface either — `all_assets` does not reach every
target a repo declares. State the surface with every number.

**A file neither side rebuilt matches vacuously.** Both are reading the same
committed bytes. Time every file against its own tree's checkout instant and
report a carried file as carried, not as agreement.

**A step's closure is whatever the tree held when it ran.** This defeats the
staleness check above, which asks whether a file was rebuilt and not whether its
inputs were the same when it was. Two builds of one step, both fresh, both
correct, are still not comparable if a shared input changed between them — so
build both sides in the same order, from one tree, and measure once.

**A binary that does not match its source invalidates the run, in either
direction.** Rebuilding while a build is in flight has twice produced a
confident wrong verdict here: once judging a correct fix broken because the
binary predated it, once nearly rejecting a correct fix because a replaced
binary made `/proc/self/exe` resolve to `… (deleted)` and broke every recipe
that spawns a second command. Prove the code is in the binary before believing
a failure — functionally where a token is not a literal.

And two rules about what to conclude:

- **A target absent from both sides needs a reason, not a tick.** It can mean the
  recipe correctly produces nothing, or that the build died leaving no error and
  no partial file. A comparison that walks the files present in both trees cannot
  see the second kind at all.
- **"The reference is non-deterministic" is a measurement, not an explanation.**
  Run it two or three times on identical input and show it disagrees with itself.
  The same command can be reproducible on a small input and not on a large one,
  so one file proves nothing either way.

### Two shapes most defects turn out to have

- **A mechanism exists and a second code path does not know about it.** A gap
  check that does not consult the resolver beside it; a debug flag instrumenting
  one of two routes; a boundary rule implemented for one operation. These rarely
  surface as a wrong value — they surface as a hard failure, or as an absent log
  line, and that second form is the dangerous one because it makes a true
  conclusion look proven by an instrument that could not have shown otherwise.
- **State attached to a document that some operation invalidates or never
  establishes.** Verbatim source blocks replayed after a filter dropped their
  subject; a marker derived from a directory that cannot tell a cache from a
  declared intermediate; a numbering pass whose axiom coverage silently omits a
  construct. The design question is not "does this state survive a write" but
  **what operation makes it a lie, and does the carrier know**.

When a rule is right for one command, ask which other commands it is right for
before implementing it once. `tests/architecture.rs` is where that question gets
a permanent answer.

## The tests that hold this up

- `tests/plan_only.rs` — plan a repo, **move its Makefile out of the tree**, and
  run the same things again. This is the acceptance test; a green suite without
  it proves nothing.
- `tests/architecture.rs` — the Makefile/ODK-yaml name may appear only in
  `src/odk/`; an environment variable that changes output must be a plan field;
  there is no second QC implementation.
- `spec.rs::round_trip_tests` — `Plan → Spec → Plan` is the identity, so a field
  ingest computes and execution reads cannot be forgotten by the serializer.
- `spec.rs::format_floor_tests` — the plan schema is digest-pinned, so changing
  it forces a decision about `PLAN_FORMAT_MIN_VERSION`.

A guard that has never been seen to fail is a guard whose failure mode is
untested — it may be passing because the bug is absent rather than because it
can see the bug. Reintroduce the defect, watch the test name it, then put it
back. That is what makes the guard evidence rather than decoration.

## There is nothing in the wild

owlmake is an experiment. **There are no committed plans anywhere, no
collaborators, and no repository depends on it.** Every `owlmake.yaml` that
exists was regenerated from an ODK repo minutes ago and will be regenerated
again.

So backward compatibility is not a constraint, and treating it as one costs more
than it saves. When a better shape presents itself:

- **rename, restructure and delete plan fields freely.** No aliases, no
  migration shims, no "accept both spellings for now". A compatibility path
  carried forward is permanent confusion bought with nothing.
- **change command behaviour to match ROBOT** without staging it. If owlmake
  currently does the wrong thing, the fix is the whole fix.
- **do not add a deprecation cycle.** Delete the thing.

Two pieces of machinery survive this, for reasons that are not compatibility:

- `PLAN_FORMAT_MIN_VERSION` and `spec::schema_digest` — the digest test exists so
  that a schema change is *noticed*, not so old plans keep loading. It caught a
  new `StepSpec` variant arriving on a merge, which is the job. Keep the floor
  honest, but bumping it is rarely the right answer while everything is 0.1.0.
- the `tests/architecture.rs` allow-lists — they guard invariants (the plan-flow
  boundary, determinism), not versions, and those matter regardless of who is
  downstream.

The corollary for judgement: prefer the change that leaves the codebase in the
state you would design from scratch, not the one that minimises churn.

### Comments

Although we functionally mirror other tooling, this tooling may not be relevant 
forever and we need to assume owlmake will be. So code comments should be written
as if owlmake is the only tooling that exists. Describe OUR behavior, not how it
relates to other tools.

Also, don't describe in comments how we achieved our current behavior (e.g. explaining
how old versions of owlmake broke something and how we fixed it). This is completely
irrelevant as old versions of owlmake were never published. We just describe what we have
now.


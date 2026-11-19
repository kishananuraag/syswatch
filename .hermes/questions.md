# Held questions — asked once, then parked

Rule: I ask, I keep working on everything that does not depend on the answer, and I **do not re-ask**.
Anything still open is answered by the next message from Kishan.

| # | Question | Asked | Blocks | Default I'm using meanwhile |
|---|---|---|---|---|
| Q1 | Sensors: `ioreg`-only, or passwordless `powermetrics` (real temp/power/GPU)? | 2026-11-11 | T5 only | build the `ioreg` path; powermetrics stays an optional "enhanced" plugin |
| Q2 | Push to GitHub allowed on verified work, or keep every commit local? | 2026-11-11 | nothing | **local commits only**, no pushes until told |
| Q3 | Cockpit: read-only, or add approve/reject buttons that wake me? | 2026-11-11 | T9 polish only | read-only first |
| Q4 | Concept graph: keep columns as edge labels (current) or promote ~105 columns to real nodes? | 2026-11-11 | nothing — A2 already shipped the pruning option | pruning (option A); column nodes are an enrichment job, not a fix |
| Q5 | Push branch `concept-layer` to origin (3 commits: merge, router fix, graph repair)? | 2026-11-11 | nothing | **local only**, no push until you say |
| Q6 | Regenerate `nodes_vectors.json` from the repaired graph (it's stale — built from the broken 5-edge graph)? | 2026-11-11 | vector search quality | leave stale until the graph stops moving |

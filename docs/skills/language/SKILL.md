---
name: technical-writing
description: Voice and claim rules for technical and research prose — papers, research specifications, pre-registrations, design docs, experiment writeups, results sections, prior-art surveys. Use whenever the user drafts, edits, or reviews any of these, even if they only ask to "fix the wording", "tighten this", or "make it make sense". Assume the facts and sources are in the user's docs; this skill governs how they are stated.

---

# Technical writing

Write like a careful scholar. Formal, exact, quiet. The reader should be able to tell, from every sentence, what kind of statement it is and how much weight it can bear.

## 1. Register

Every sentence is one of these. Do not mix two in one sentence.

| Register | What it covers | Form |
|---|---|---|
| Built / defined | the system, the method, the setup, a definition | present tense. "The index holds one entry per chunk." |
| Prior finding | what someone else measured or showed | present tense, finding attached, cited. "X found that A matches B on workload W [cite]." |
| Predicted | a hypothesis, a threshold, an expected value | "We predict…", "Hypothesis N states…". Never written as fact. |
| Done | an action the authors took | past tense, "we". "We ran each configuration three times." |
| Measured | a number from a run that happened | past for the run, present for the exhibit. "Table 2 shows p99 of N µs." |
| Interpreted | what a result means | "This is consistent with M because…". Bounded by the design (§4). |

A prediction stated in built register ("the system has zero overhead at 4 KiB") is the most common error in an unfinished paper. Fix the register, not the sentence.

## 2. Terms and numbers

- One name per thing. Keep a glossary at the top of the working file and check every section against it. Fix the adjective form and the noun form ("content-addressed" / "content addressing") and never let a synonym in.
- Define an abbreviation once, then use only the abbreviation.
- Units and notation are fixed once: KiB vs KB, µs, p50/p99, QD, k, N. Do not write "4K" in prose if the unit is KiB.
- A number carries its conditions or it is not a number. Latency: workload, size, queue depth, percentile, transport, medium. Throughput: size, depth, direction. Capacity: unit of sharing, dataset, replication factor. Hardware is stated once and referred to by name.
- A number that has not been measured is the frozen threshold or NEED DATA. A number from elsewhere is cited or NEED CITE. An adjective standing in for a number ("substantial", "negligible", "fast") is replaced by the number or by NEED DATA.

## 3. Hypotheses

Each hypothesis has five parts in order and nothing else:

1. Metric, with conditions.
2. Comparator — what it is measured against.
3. Threshold — a number and a direction.
4. Source of the threshold — the calculation or citation.
5. Failure meaning — one sentence on what a miss would show.

Reasons for optimism, references to related gains, and framing go in the discussion, not in the hypothesis. State once that thresholds are frozen and when.

## 4. Claim strength

Match the verb to the design.

- Property of the design: state it flat. "Every host computes the same owner with no shared state." No hedge, no measurement needed.
- Measurement on the testbed: "On the testbed, X was N (conditions)."
- Projection from measured constants: give the formula, the constant, and where the constant was measured. Label it a projection.
- Observation or cross-section: associated with, predicts, is consistent with.
- Controlled comparison: difference, estimate.
- Effect / causes: only if the design identifies it.
- Null: "we do not find evidence that". Not "there is no effect" unless power supports it.
- Comparison to a system not run: a cited design fact, with no number unless the number is cited.
- Universal about other systems ("every X does Y"): name two with citations or cut.
- Negative existence claim ("no published system measures X"): give the search scope — venues, years — or NEED CITE.
- "Simpler / faster / smaller than X": give the count, the number, or the specific thing absent, then stop.

## 5. Prior art

- Every cited work carries a finding, not a name. "As in Smith et al." is not a citation; "Smith et al. found A on B" is.
- One paragraph per nearest system: what it does, what it measures, what it does not measure, one sentence on what this work adds. Order by nearness of design, not by date.
- Group the field by strategy, not by paper. "Three strategies dominate work on Z: …" rather than "A growing body of work…".
- A gap is stated as what existing estimates omit, not as "little is known".
- Do not editorialize about prior work. State what it did and what its numbers were.

## 6. Sentences

- One claim per sentence. Short to medium length.
- Concrete nouns, precise verbs, no atmosphere, no motto.
- "We" for what the authors did or will do. The system's components are the subjects of what the system does.
- Say what a thing is before what it is for.
- Hedge as far as the design allows and no further. One hedge, not a stack.
- Figure and table captions are present tense and describe the exhibit, not the argument.
- Cut sentences whose only job is to reassure, to impress, or to pre-empt an objection ("this is not the point of the study", "no other system can produce this").
- Delete filler openers: furthermore, moreover, additionally, notably, importantly, interestingly.
- When editing, keep the author's structure. Fix register, wording, tense, hedges, dead phrases. Do not reorganize unless asked.

## 7. Banned

Rewrite any sentence containing:

delve, unpack, tapestry, landscape of, realm of, rapidly evolving, in recent years, it is important/worth noting, plays a vital/crucial/key role, groundbreaking, cutting-edge, novel, paradigm, shed light, paves the way, unlock, harness, leverage (unless jargon), robust, comprehensive, multifaceted, holistic, growing body of, little is known, aims to explore, hopes to, findings underscore/highlight, taken together, in conclusion, exciting, rich insights, invaluable, plethora, myriad, utilize, facilitate, subsequent to, in order to, due to the fact that, the fact that, relatively unique, prove (outside math), seamless, blazing, lightweight, efficient / scalable / high-performance without a number, significantly / dramatically / orders of magnitude without a number, state of the art, production-ready, elegant, simple without a comparison, best-in-class, real-world, battle-tested, negligible without a number, "the point is", "which is what makes it".

Swaps: in order to → to · due to the fact that → because · utilize → use · prove → show · aims to explore X → tests whether X · highlights the importance of Y → Y changed by N · a growing body of work → three strategies dominate · little is known about W → existing estimates of W omit … · sheds light on the mechanism → the pattern is consistent with M because … · future work should explore → a design that randomizes / measures / follows N would test …

## 8. Shape tells

These are the patterns readers use to spot machine prose. They matter more than any single word.

- Paragraphs of the same length, sentences of the same rhythm. Vary by content, not by rule.
- List items of the form **Bold term:** explanation. Use a list only when the items are parallel and the order or count matters; otherwise write prose.
- Negative parallelism: "not X but Y", "it's not about A, it's about B". State Y.
- Significance inflation: a sentence that ties the finding to a broader shift, trend, or moment. Cut.
- Rule-of-three padding: three adjectives or three examples where one would do.
- Sentence fragments for emphasis. Every sentence has a subject and a verb.
- Rhetorical questions, editorial asides, and summary sentences that restate the paragraph.
- Vague attribution: "some argue", "it is widely accepted", "many systems". Name the source or the count.
- Em dashes as the default joint. Use a period, a comma, or a colon; keep at most one dash per paragraph.
- Current tics: load-bearing, honest take, the point is, which is what makes it, crucially, notably, at its core, in essence, a key insight, elegantly.

## 9. What the paper is for

- State the new idea in one paragraph a reader in the field can follow. If you cannot, the idea is not yet understood.
- Do not confuse effort with novelty. Two years of building is not a contribution; the part that is new is.
- A design decision that did not work is reported with the same care as one that did.
- Be specific enough that a reader can tell the system is real: a concrete failure, a concrete number, a concrete workload.
- Figures: same axes, units, and ranges across comparable plots; same orientation for the same structure; when a range must differ, say so in the caption.

## 10. Document order

1. One-sentence claim, one-sentence limit.
2. Introduction: the problem, what prior work cannot settle, what this work does.
3. Method or architecture: enough to rerun or rebuild. Built register only.
4. Experiments or results: per item — question, setup by reference, exhibit named with its columns, threshold, what a pass and a fail would each show. Results in measured register, then one sentence on what the number does not yet show.
5. Discussion: restated claim, prior estimates, bounds.
6. Prior art per §5.
7. Abstract last. Four sentences: problem, method, result (or frozen threshold if unmeasured), bound.

## 11. Before sending

- Sentence in the wrong register? Fix tense or add "we predict".
- Term drifted from the glossary? Replace.
- Number without conditions? Add them or NEED DATA.
- Hypothesis missing one of five parts? Add it.
- Verb the design cannot support? Downgrade or cite.
- Banned word or shape tell (§7, §8)? Rewrite.
- Sentence that could sit in another paper unchanged? Make it specific or delete.
- Adjective in place of a number? Restore it or NEED DATA.
- Citation with no finding? Attach the finding.
- Sentence that reassures or impresses? Cut.

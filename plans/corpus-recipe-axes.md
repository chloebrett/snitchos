# Corpus recipe axes — 100 domains and a sampled crossing each

**Status: 📐 DATA — shipped as `cram-gen/assets/recipes/batch9.toml`, and
superseded for new batches.** Axis values for [corpus-mvp.md](legacy/corpus-mvp.md)'s
Increment 3 recipe-tuple generator. Split out of that plan to keep it readable;
the *rules* for crossing these axes live there, the *values* live here.

**The 100 below are batch9's sheet and are frozen.** They are the record of what
produced `corpora/batch9`, so the findings in
[../notes/batch9-findings.md](../notes/batch9-findings.md) can still be read back
onto the axes that produced them. New batches draw from
`cram-gen/assets/recipes/batch10.toml` — 500 domains, each asked for at two
different crossings, flattened so a run covers every domain before it repeats
one. Pick a sheet with `cargo xtask cram --gen --recipes <name>`; the default is
the newest. The rules on this page (clauses, declaration-counted sizes, the
size↔construct-count crossing rule) all still hold; what changed is the size mix
and the size latitude, both from Finding 1 of the batch9 notes — parse-death is
a monotone function of program length, so batch10 skews small and its briefs no
longer end "if the program naturally wants to be bigger, let it be".

Distinguishing clauses are model-generated (Claude) and unreviewed. **Read them
before use** — the whole point of the clause is that it names the real
computation, and a clause that names the wrong one will steer the model wrong for
every program in that domain.

---

## The non-domain axes

**Constructs** (from [../docs/language-design.md](../docs/language-design.md)):
`prod` · `sum` · `contract` + `on` · `uses` capabilities · `Result` with `?` ·
`Maybe` · `|>` · `use <-` · placeholders · recursion · `List`/`Map` ops.
`test` blocks are always required, so they are not a construct axis.

**Sizes — counted in declarations, not lines.** A model cannot count lines and
wrecks a working program trying to
([corpus-mvp-spike.md](corpus-mvp-spike.md) Findings 004). Types and functions it
*can* count, at trivial cost, and the ranges are wide and overlapping so they
signal scale without becoming a target to trim toward:

| Bucket | Types | Functions |
|---|---|---|
| small | 1–4 | 2–6 |
| medium | 2–6 | 4–10 |
| large | 3–10 | 8–20 |

Always state that **tests are extra and do not count**, and that the range is a
guide rather than a limit. Actual line counts are a harness metric, measured
after the fact — never a constraint the model has to satisfy.

**Shapes**: module (`ext` items, no `main`) · script (has `main`) · server loop
(receive → dispatch) · library-with-heavy-tests.

**The one hard crossing rule**: construct count scales with size — small 1–2,
medium 2–3, large 3–4.

---

## Why each domain carries a clause

A bare domain name lets a weak model default to the same
records-with-timestamps-plus-filter program for every entry. The clause names the
actual computation, which is what makes `sauna booking` (interval overlap),
`ice rink session booking` (capacity over a recurring timetable) and
`library hold queue` (FIFO per title with expiry) diverge into three genuinely
different programs rather than one wearing three hats.

Clause-writing is **one-off and amortised** over the whole corpus run, not paid
per generation. Writing the remaining clauses for any domains added later is also
a good English-only smoke test for the local model, exercising prompt, runner,
cache and extraction before Stitch validity is in play.

## ~~Must-use words~~ — axis REMOVED

**The must-use-identifiers axis is dead.** It was borrowed from TinyStories §2.1,
where three random words are injected per prompt to force lexical variety. Five
candidates of evidence say the mechanism **does not port from prose to code**
([corpus-mvp-spike.md](corpus-mvp-spike.md) Findings 002, 003, 005):

| | Outcome |
|---|---|
| 002 | `loyly` deadlocked a 4B for 2.5 minutes — no program at all |
| 003 | `cooldown` became a permanently-zero field; the others reached comments only |
| 005 | the words displaced the meaningful function names — `overlaps` → `birch` |

Every candidate also spent a large share of its reasoning budget deciding where to
put them.

**Why it cannot work.** Prose absorbs an arbitrary noun at no cost — a story can
be about a *lantern* whether or not lanterns were the point. **Code identifiers
carry semantics: a name that does not describe its referent is a defect, not a
variation.** Forcing three unrelated words into a 50-line module produces dead
bindings, meaningless field names, or — worst — good functions with names that
lie about them. Training a corpus on that teaches the model to name things
badly.

**Lexical diversity has to come from the axes that change what the program
does** — domain, constructs, shape. Those move the identifiers as a *consequence*
of moving the program, which is the only way identifier variety is also
identifier quality.

Two findings worth keeping from the experiment, in case a future variant is
attempted: rare words with a **high-frequency near-neighbour** (`loyly` →
`loyalty`) are deadlock traps, because the token prior pulls to the neighbour and
self-correction loops; and a **hard** constraint a small model cannot satisfy
deadlocks where a soft one degrades — but neither phrasing rescued the axis.

The word lists are retained in the 100 entries below as a record of the
experiment. **They are not part of a recipe** and should not be rendered into a
prompt; see [prompt v4](corpus-prompts/v4.md), which replaces
that line with "Name things for what they do."

---

## The 100

Format: **domain** — distinguishing clause / `{constructs}` · size · shape ·
*must-use words*. One sampled crossing per domain; the sampler will produce many
more per domain, these are a review artifact and a starting seed.

### Inventory & stock

1. **warehouse bin allocation** — smallest bin that fits; fragmentation matters
   `{prod, Map, |>}` · medium · module · *tote, dunnage, slotting*
2. **bakery inventory** — daily bake against sell-through; staleness expires stock
   `{prod, Maybe}` · small · module · *proof, sheet, daypart*
3. **seed catalogue** — germination rates and sowing windows per variety
   `{prod, sum, Result+?}` · medium · library-with-heavy-tests · *cultivar, scarify, chit*
4. **spare-parts bin** — reorder points triggered by consumption rate
   `{prod, |>}` · small · module · *bin, leadtime, consignment*
5. **tool crib checkout** — who holds what, and overdue detection
   `{prod, Maybe, Map}` · medium · module · *crib, gauge, calibration*
6. **lost-property office** — match found items against claims by description
   `{sum, prod, recursion}` · medium · module · *claimant, ticket, custody*
7. **pharmacy stock rotation** — first-expiring-first-out, not first-in-first-out
   `{prod, Result+?, uses Telemetry, |>}` · large · script · *lot, potency, formulary*
8. **cold-chain shipment log** — temperature excursions invalidate a shipment
   `{prod, sum, uses Telemetry}` · medium · server loop · *excursion, reefer, probe*
9. **shipping container stowage** — weight distribution and stacking constraints
   `{prod, recursion, Result+?, Map}` · large · module · *tier, lashing, deadweight*
10. **museum exhibit rotation** — light exposure budgets limit display time
    `{prod, Maybe, |>}` · medium · module · *lux, accession, vitrine*

### Queues, bookings & rotas

11. **library hold queue** — one FIFO queue per title; holds expire if not collected
    `{prod, Result+?, |>}` · medium · module · *shelf, patron, expiry*
12. **kitchen order queue** — courses fire at different times so a table lands together
    `{sum, contract+on, |>}` · large · server loop · *ticket, expedite, rail*
13. **barber shop appointments** — variable service durations against chair availability
    `{prod, Maybe}` · small · module · *chair, walkin, fade*
14. **court docket** — priority and continuance push cases down the list
    `{sum, prod, recursion}` · medium · module · *continuance, arraign, cause*
15. **classroom timetable** — no room, teacher, or class double-booked
    `{prod, Map, Result+?, recursion}` · large · module · *period, cohort, clash*
16. **on-call rotation** — fair distribution with rest periods between shifts
    `{prod, contract+on, |>}` · medium · module · *handover, escalation, respite*
17. **ice rink session booking** — capacity per recurring slot, many skaters per session
    `{prod, Maybe, Map}` · medium · module · *zamboni, session, freestyle*
18. **allotment plot waiting list** — seniority ordering with plot-size preferences
    `{prod, |>}` · small · module · *rod, tenancy, halfplot*
19. **community hall bookings** — recurring bookings collide with one-offs
    `{prod, sum, Result+?}` · medium · library-with-heavy-tests · *recurrence, hirer, deposit*
20. **telescope observing queue** — targets ranked by visibility window and priority
    `{prod, contract+on, |>, uses Telemetry}` · large · server loop · *airmass, meridian, seeing*

### Transit & movement

21. **subway turnstile** — fare deduction with entry/exit pairing and stuck-open faults
    `{sum, prod, uses Telemetry}` · medium · server loop · *tap, interchange, tailgate*
22. **ferry schedule** — tide-dependent sailings and vehicle deck capacity
    `{prod, Maybe, Map}` · medium · module · *linkspan, sailing, laneMetre*
23. **flight manifest** — seat assignment with weight-and-balance limits
    `{prod, Result+?, recursion, |>}` · large · module · *trim, pax, hold*
24. **parking garage occupancy** — counts per level with oversize-vehicle rules
    `{prod, Map, |>}` · medium · module · *bay, tandem, clearance*
25. **bus stop departure board** — predicted vs scheduled times, with cancellations
    `{sum, prod, uses Telemetry}` · medium · server loop · *headway, layover, curtail*
26. **bike share dock** — rebalancing when docks are full or empty
    `{prod, Maybe}` · small · module · *dock, rebalance, spur*
27. **level crossing barrier** — a safety interlock state machine
    `{sum, contract+on, uses Telemetry}` · medium · server loop · *interlock, approach, wigwag*
28. **taxi meter** — a state machine over distance and waiting time, with tariff changes
    `{sum, prod, uses Telemetry}` · small · script · *fare, surcharge, hiring*
29. **toll booth** — class-based tariffs and unpaid-toll accumulation
    `{prod, sum, Result+?}` · medium · module · *axle, gantry, violation*
30. **elevator dispatch** — assign cars to calls to minimise waiting
    `{sum, contract+on, recursion, Map}` · large · server loop · *hoistway, landing, nudge*

### Sensors, meters & logs

31. **weather station** — rolling aggregates with gap handling in the series
    `{prod, Result+?, use <-}` · medium · script · *barometer, lull, calibrate*
32. **tide table** — harmonic prediction and high/low extraction
    `{prod, recursion, |>}` · medium · module · *ebb, springs, datum*
33. **seismograph log** — event detection above a noise floor
    `{prod, sum, |>}` · medium · module · *tremor, epicentre, magnitude*
34. **water meter readings** — deltas between cumulative readings, including rollover
    `{prod, Maybe, |>}` · medium · module · *dial, rollover, leakage*
35. **air quality monitor** — index bands with threshold-crossing alerts
    `{sum, prod, uses Telemetry}` · medium · server loop · *particulate, band, exceedance*
36. **soil moisture probe** — irrigation decisions from depth-layered readings
    `{prod, Map}` · small · module · *tensiometer, horizon, wilting*
37. **beehive scale** — nectar flow inferred from daily weight change
    `{prod, |>}` · small · module · *nectar, tare, supering*
38. **river gauge** — flood stage thresholds and rate-of-rise warnings
    `{sum, prod, uses Telemetry, |>}` · large · server loop · *stage, freeboard, bankfull*
39. **street light fault log** — fault clustering suggests a common circuit
    `{prod, Map, recursion}` · medium · module · *lantern, feeder, photocell*
40. **whale sighting log** — sightings grouped into encounters by time and place
    `{prod, recursion, |>}` · medium · module · *breach, fluke, pod*

### Money & ledgers

41. **household ledger** — double-entry balancing; categories roll up
    `{prod, Result+?, recursion, Map}` · large · library-with-heavy-tests · *posting, reconcile, envelope*
42. **tip pooling** — hours-weighted distribution with rounding remainder
    `{prod, Result+?, |>}` · small · module · *tronc, shift, gratuity*
43. **market stall takings** — cash reconciliation against recorded sales
    `{prod, Maybe}` · small · module · *float, pitch, takings*
44. **subscription billing** — proration on mid-cycle plan changes
    `{prod, sum, Result+?}` · medium · module · *proration, dunning, lapse*
45. **currency exchange board** — cross-rate derivation with spread
    `{prod, Map, |>}` · medium · module · *spread, cross, pip*
46. **petty cash box** — receipts must reconcile the float
    `{prod, Result+?}` · small · library-with-heavy-tests · *chit, imprest, float*
47. **invoice aging** — bucketing by days overdue with escalation
    `{prod, sum, |>}` · medium · module · *ageing, dunning, remittance*
48. **vending float reconciliation** — coin denominations against sales
    `{prod, recursion, Map}` · medium · module · *hopper, denomination, escrow*
49. **locker rental** — periods, deposits and abandonment
    `{prod, sum, Maybe}` · medium · module · *tenure, forfeit, deposit*
50. **library fine amnesty** — recompute balances under a retroactive policy
    `{prod, contract+on, |>}` · medium · library-with-heavy-tests · *amnesty, accrual, waiver*

### Games & scoring

51. **chess clock** — increment and delay modes, flag detection
    `{prod, Maybe, recursion}` · small · module · *increment, flag, tempo*
52. **darts scoring** — checkout paths from a remaining score
    `{sum, recursion, |>}` · medium · module · *checkout, treble, bust*
53. **cribbage board** — hand scoring combinatorics (fifteens, runs, pairs)
    `{sum, recursion, Map, |>}` · large · library-with-heavy-tests · *peg, fifteens, nobs*
54. **bowling scorecard** — strikes and spares pull forward later frames
    `{sum, recursion}` · medium · library-with-heavy-tests · *frame, spare, tenth*
55. **sudoku grid** — constraint propagation on candidate sets
    `{prod, recursion, Map, Result+?}` · large · module · *candidate, peer, naked*
56. **crossword grid** — slot extraction and intersection consistency
    `{prod, Map, recursion}` · medium · module · *light, clue, checked*
57. **dominoes train** — matching ends: constrained graph traversal
    `{sum, recursion, contract+on}` · medium · module · *pip, spinner, boneyard*
58. **go territory scoring** — flood-fill regions; distinguish alive groups from dead
    `{sum, recursion, Map, |>}` · large · module · *liberty, seki, atari*
59. **pinball high scores** — initials table with tie-breaking rules
    `{prod, Maybe}` · small · module · *initials, tilt, replay*
60. **tournament bracket** — seeding, byes and advancement
    `{sum, recursion, prod}` · medium · module · *bye, seeding, consolation*

### Sport & outdoors

61. **swim lane assignment** — seed times into lanes, fastest in the middle
    `{prod, |>}` · small · module · *heat, seedtime, lane*
62. **marathon split times** — pace projection from partial splits
    `{prod, Maybe, |>}` · medium · module · *split, negative, chip*
63. **rowing crew seating** — balance port and starboard by side preference
    `{prod, sum, recursion}` · medium · module · *stroke, bowside, rigging*
64. **referee assignment** — avoid conflicts of interest and travel clashes
    `{prod, Map, Result+?, recursion}` · large · module · *fixture, neutrality, appointment*
65. **ski patrol incidents** — triage ordering and resource dispatch
    `{sum, contract+on, uses Telemetry}` · medium · server loop · *toboggan, piste, triage*
66. **climbing route grading** — consensus grade from a set of opinions
    `{prod, sum, |>}` · medium · module · *crux, sandbag, onsight*
67. **campsite pitch allocation** — pitch size and hookup requirements
    `{prod, Maybe, Map}` · medium · module · *pitch, hookup, hardstanding*
68. **orienteering control points** — validate a punched course in order
    `{prod, recursion, Result+?}` · medium · library-with-heavy-tests · *punch, leg, bearing*
69. **sauna booking** — exclusive occupancy, so overlapping intervals must be detected
    `{prod, Maybe, |>}` · small · module · *cedar, birch, cooldown*
70. **hiking trail register** — sign-in/sign-out pairing to find overdue parties
    `{prod, sum, Maybe}` · medium · module · *bothy, overdue, party*

### Growing & livestock

71. **greenhouse watering** — zone schedules adjusted by recent rainfall
    `{prod, contract+on, uses Telemetry, Map}` · large · server loop · *zone, evapotranspiration, misting*
72. **bird feeder log** — species counts with seasonal comparison
    `{prod, Map, |>}` · medium · module · *sparrow, suet, dusk*
73. **orchard harvest** — per-tree yields and picking-window scheduling
    `{prod, Maybe, |>}` · medium · module · *rootstock, brix, windfall*
74. **sheep flock register** — lineage tracking and lambing records
    `{prod, recursion, Map}` · medium · module · *tup, ewe, tagging*
75. **mushroom cultivation** — substrate batches through contamination checks
    `{sum, prod, Result+?}` · medium · module · *spawn, flush, pinning*
76. **compost turning** — carbon-nitrogen ratio and turn scheduling
    `{prod, |>}` · small · module · *browns, greens, thermophilic*
77. **apiary inspection** — queen status and disease signs per hive over time
    `{prod, sum, uses FsWrite, Map}` · large · library-with-heavy-tests · *queenright, varroa, brood*
78. **fish hatchery tank** — stocking density against growth rates
    `{prod, Result+?}` · small · module · *fry, biomass, grading*
79. **tree ring measurement** — series cross-dating between cores
    `{prod, recursion, |>}` · medium · module · *core, sapwood, drought*
80. **seed bank vault** — viability testing intervals per accession
    `{prod, Maybe, Map}` · medium · module · *accession, viability, desiccant*

### Craft, food & making

81. **knitting pattern rows** — stitch-count arithmetic across shaping rows
    `{prod, recursion, Result+?}` · medium · library-with-heavy-tests · *decrease, gauge, raglan*
82. **pottery kiln firing** — ramp, soak and cooling schedules per clay body
    `{sum, contract+on, uses Telemetry}` · medium · server loop · *bisque, ramp, soak*
83. **loom warp threading** — threading and treadling from a draft
    `{prod, Map, recursion}` · medium · module · *heddle, shaft, treadle*
84. **woodworking cut list** — cutting plan minimising offcut waste
    `{prod, recursion, |>, Result+?}` · large · module · *kerf, offcut, rip*
85. **bookbinding signatures** — folding and nesting arithmetic; page order is non-obvious
    `{prod, recursion}` · medium · module · *folio, gather, spine*
86. **dyeing lot tracking** — batch consistency across dye lots
    `{prod, Maybe}` · small · module · *dyelot, mordant, skein*
87. **model railway layout** — block occupancy and route setting
    `{sum, Map, contract+on, recursion}` · large · server loop · *block, turnout, interlocking*
88. **brewery fermentation** — a time series against target curves, with alerts
    `{prod, contract+on, uses Telemetry}` · large · script · *krausen, gravity, pitch*
89. **coffee roast profiles** — first-crack timing and development ratio
    `{prod, |>}` · small · module · *crack, development, drop*
90. **cheese ageing cave** — turn and wash schedules by wheel age
    `{prod, uses Telemetry, Result+?}` · medium · script · *rind, affinage, turn*

### Media, records & home

91. **playlist shuffling** — avoid repeating artists too closely
    `{prod, recursion, |>}` · medium · module · *rotation, separation, segue*
92. **podcast feed** — episode ordering with seasons and specials
    `{prod, sum, Maybe}` · medium · module · *enclosure, season, trailer*
93. **subtitle timing** — interval arithmetic with shift and stretch
    `{prod, Result+?, |>}` · medium · library-with-heavy-tests · *cue, framerate, drift*
94. **photo album tagging** — tag hierarchies and set queries
    `{prod, Map, recursion, |>}` · large · module · *facet, roll, sidecar*
95. **radio station rotation** — rotation categories with separation rules
    `{prod, contract+on, Map}` · medium · module · *rotation, daypart, burn*
96. **e-reader bookmarks** — position sync across devices with conflicts
    `{prod, sum, Result+?}` · medium · module · *anchor, furthest, conflict*
97. **thermostat schedule** — setpoints with hold and override precedence
    `{sum, prod, Maybe}` · medium · module · *setpoint, hold, deadband*
98. **laundromat machine status** — cycle timing and queue notification
    `{sum, prod, uses Telemetry}` · medium · server loop · *drum, lint, cycle*
99. **dog licence register** — renewal windows and lapse handling
    `{prod, Maybe, |>}` · small · module · *chip, breed, lapse*
100. **lost pet register** — match reports to sightings by area and description
     `{prod, recursion, Map, Maybe}` · large · module · *sighting, microchip, radius*

---

## Distribution of this sample

Sizes: 24 small · 56 medium · 20 large. Shapes: 62 module · 14 server loop ·
5 script · 11 library-with-heavy-tests, **which is lopsided** — the sampler
should draw shapes uniformly, and this seed under-represents `script` badly.
Treated as a review finding rather than a target distribution.

~~Every crossing above respects the size↔construct-count rule.~~ **Five do not** —
`taxi meter`, `tip pooling`, `chess clock`, `sauna booking` and
`dog licence register` are all `small` asked for three constructs. The claim was
written, never checked, and only surfaced when batch10 made the rule a test. They
stay in `batch9.toml` because that sheet is the record of what produced the
corpus; the test pins them so the count cannot grow.

None has been validated by generating a program.

## What batch10's sheet does differently

Validated now — 973 programs' worth. `cram-gen/assets/recipes/batch10.toml`
carries these 100 domains forward with their clauses intact, re-crossed, plus 400
new ones, and changes four things:

| | batch9 | batch10 |
|---|---|---|
| domains | 100 | 500 |
| rows | 100 (one crossing each) | 1000 (two crossings each) |
| repeats | same domain **at the same crossing**, ten times | same domain at two different crossings |
| sizes | 24 small · 56 medium · 20 large | 500 small · 462 medium · 38 large |
| shapes | 62 module · 14 server · 5 script · 11 library | 447 module · 162 server · 158 script · 233 library |
| latitude | "if the program naturally wants to be bigger, let it be" | "a longer program is not a better one" |
| opening line | "Write a `<domain>` **module**", whatever the shape | names the shape: module / script / service / library |

The crossings flatten **pass-major** — every domain's first crossing, then every
domain's second — so a 500-candidate run sees 500 distinct domains rather than
250 of them twice. The eight domains Finding 4 found to be zero-yield
(`sudoku grid`, `bowling scorecard`, `go territory scoring`, `darts scoring`,
`cribbage board`, `orienteering control points`, `playlist shuffling`,
`woodworking cut list`) are **kept deliberately**: the finding is that Stitch
cannot express indexed grid iteration in a program that survives its own length,
and asking for a smaller program is a different experiment rather than a rerun of
the same one. Their yield at `small` is the thing to look at in batch10.

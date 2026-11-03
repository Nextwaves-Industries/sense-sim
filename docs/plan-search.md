# Find the best plans

The Deploy card's main button for UWB, and the assistant's `rtls_smart_plan` tool. Auto deploy
answers one question: where do the anchors go, for this floor as it stands. This asks what a
customer asks, and tests its own answer before giving it.

Code: `src/rtls/smart.rs` (the job), `src/rtls/flow.rs` (traffic), `src/rtls/smart_ui.rs` (the
card), `src/rtls/placement.rs` (the search and its figures).

## What it does differently

1. **Judges a plan where tags go.** Traffic is worked out from the layout: trips between doors,
   staging and rack faces, down the middle of the aisles. Routes you drew count for more than the
   guess. A point weighs what its traffic does, and never nothing. It's a model of a layout, not a
   measurement of a site.
2. **Searches across bad days, not just checks them afterwards.** A busy shift, materials 50 %
   worse and racks half full are in the search itself, and a plan is judged on its worst day. The
   figures match `site_stress`, so the two can be read together. They're assumptions about a
   typical site, not this site's records.
3. **Knows what a good average hides**, each checked against the simulator's own measurements:
   - *Enough links at the same moment.* Five links that each work 60 % of the time look fine one by
     one, and leave a tag with no position a third of the time. `Acc::available`: predicted against
     simulated, 0.976/0.966, 0.806/0.806, 0.483/0.472 and 0.104/0.105 from 2 to 20 people per 100 m².
   - *Enough links the solver trusts.* A person in a link gets it flagged and all but ignored, so
     with too few left the position is poor even though everything answered. `Acc::dependable`:
     0.853/0.853, 0.606/0.616 and 0.277/0.289 from 5 to 20 people, with error 10-16 cm predicted
     against 11-19 cm simulated in those moments, and 41-59 cm in the others.
   - *Anchors in a line.* They can't say which side of the line a tag is on. The accuracy figure
     can't see it, since the wrong position fits as tightly as the right one, and a parked tag stays
     wrong: errors of about 3.5 m in the hospital ward sample. `Acc::on_one_line`.
4. **Offers choices, and lets the simulator decide them.** It finds the best set at each size, then
   walks that curve in the simulator: prediction only picks where to start. Lean, Balanced and
   Bulletproof are named for what they were measured to do, not what a search was aiming at. Plain
   Auto deploy is tested the same way alongside, and gets recommended when it's the right answer.
5. **Says why each anchor is there**: the tag time it holds up, on the day it counts for most, near
   what.
6. **Says what the site asks of the antenna.** The cone each mount serves, the least gain towards
   each band, and how much range shift the plan has room for. *Test this antenna* solves a design
   against that cone (`design_uwb_bias` with `theta_min`/`theta_max`). A catalogue pick is judged
   against the horizon, whatever the site.

## How results are kept honest

- Every plan gets the same routes, spots and seeds.
- A reported figure is the worst of three runs, from runs that took no part in choosing the plan.
- A missing result is a miss. Nothing tested holds nothing.
- **Anywhere on the floor**: a tag stands at 30 spots drawn evenly over the floor, used or not. A
  plan built around routes can't hide what it leaves out: under 24 of 30 and it isn't recommended.
  That line is a judgement.
- If the scene changes while it runs, or before an option goes on the floor, the plans are dropped
  rather than applied to a layout they weren't tested on.

## What it found

Two seeds per site, 30 cm target, debug build, 2026-11-03. Figures are p95 in cm; *gaps* means more
than 5 % of updates had no position. ✗ misses the target. "Dead" is the anchor the plan leans on
most. **Simulated. None of this has met real hardware.**

| Site | Today's Auto deploy | Recommended |
|---|---|---|
| Small warehouse | 8 anchors. Busy 31-32 ✗, dead: gaps ✗ | 7 anchors. Worst day 28, dead 22, floor 28-29/30 |
| Big warehouse, seed 1 | 28 anchors. As modelled 367 ✗, busy: gaps ✗, dead 371 ✗ | 20 anchors. Worst day 28, floor 29/30, dead 371 ✗ |
| Big warehouse, seed 2 | 23 anchors. Busy: gaps ✗, dead: gaps ✗ | 25 anchors. Racks half full 31 ✗, the rest 22 or better, dead 16 |
| RFID warehouse, seed 1 | 6 anchors. Busy: gaps ✗, dead: gaps ✗ | 9 anchors. Worst day 22, dead 15, floor 30/30 |
| RFID warehouse, seed 2 | 6 anchors. Busy: gaps ✗, dead: gaps ✗ | 8 anchors. Busy 39 ✗, the rest 14, dead 16 |
| Workshop | 8 anchors. Busy 48-51 ✗, dead: gaps ✗ | 10 anchors. Worst day 26, dead 25-26, floor 30/30 |
| Office | 13 anchors. Holds everything, floor 27/30 | The same plan: nothing found beats it. A 5-anchor option holds the routes, at 22-23/30 of the floor |
| Hospital ward | 23 anchors. 346 ✗ (seed 2 on a busy shift: 33 ✗) | 20 anchors. As modelled 26, busy 47 ✗, dead 68-134 ✗. **Nothing found holds here** |

Of the 12 runs, the recommended plan held every day along the routes and the floor check in 8 (2 of
them plain Auto deploy's own, in the office), and today's Auto deploy held every day along the
routes in 2. It also costs anchors: more than today's plan on the RFID warehouse and workshop, and
on one of the two big warehouse runs. Fewer on the others. The answer varies with the seed near the
line, as the RFID warehouse's 8 and 9 show.

## What it doesn't do

- UWB only. Other technologies still go through Auto deploy.
- Traffic, the bad days and the floor line are assumptions. None is fitted to a site.
- "Dead anchor" takes out the one anchor the plan leans on most, on an ordinary day, not every
  anchor in turn. Your own anchors aren't tried.
- Mirror positions are only caught when everything heard is close to one line, and tags in walled
  rooms remain the hard case: see the hospital.
- It doesn't pick antennas per mount. UWB power is limited at the antenna's output, so gain doesn't
  extend a legal two-way range, and the link budget doesn't model that limit yet.
- Plain Auto deploy's step still runs in one go: the longest wait measured was 22 s on the big
  warehouse in a debug build, and the whole job took up to about 4 minutes there. A release build
  hasn't been timed.

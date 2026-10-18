# Graph layout: findings and algorithms

Notes from the layout experiments of 2026-10-18, so the work can be picked up later.
Code: `tools/layout-experiments/` (Python, throwaway; the product version is to be written in
Rust in `crates/layout`). What is already in the product is listed under "State".

## State

In the product (`crates/layout`):
- `initial.rs`: a layered (Sugiyama-style) layout, used for a batch that creates 3 or more
  nodes without `x`/`y` (`crates/server/src/ops.rs`, `lay_out_deferred`). It gives a long,
  thin drawing on deep graphs (the 36-node test graph came out 4360 x 531 px, 8:1), which the
  user rejected as "too elongated". **Replace it** with the pipeline below.

Not built yet (the user asked for all four, "overwrite everything"):
1. The clustered force-directed layout in Rust (deterministic: fixed seeds, several restarts).
2. The edge pass and the label-fit rules.
3. A `tidy` op (and MCP tool, and a UI button) that applies it to the whole graph, overwriting
   every position; the op may take the viewer's aspect ratio (default 16:9).
4. Using it for new batches in place of the layered layout.

## The test graph

"Synthline" on waypoint.qdat.net (36 nodes, 57 links, one edge type whose label is "data from",
node types: external, ingress, stage, store, model, egress). Edit key in the conversation; the
experiments wrote positions straight into the shared Yjs document (`ydoc.cjs set`), because
`move_node` batches get distorted by make-room and edge nudging (only 2 of 36 nodes ended where
planned). The original positions are in `backup-positions.json` of the job directory (not in
git); restoring needs `ydoc.cjs set` with them (anchors are not restored: every node was written
as absolute, anchor-less).

## What the user wants (accumulated requirements)

- Fits a **16:9** screen at about normal zoom. Not elongated.
- Compact, but not "too dense" (an earlier version at ~1300 x 790 was rejected as too dense).
  The size is dominated by the edge-text rule below, not by node packing.
- Group by node type (the clustered variant was "very good"; plain force-directed was "good one,
  definitely keeper" but clustering was liked more).
- **No node may sit on an unrelated link**, but a link may overlap **up to a third** of a node.
- **Edge text must fit** between its two nodes. The client hides a label when the visible link
  (box edge to box edge) is shorter than `label_width + 2*ARROW_LEN + 8`
  (`client/src/main.ts`, `ARROW_LEN = 10`). With "data from" at 11 px that is about 86 px.
- **No node may cover any edge text.**
- **Edge texts may not overlap each other.**
- Flow direction (dependencies left of dependants) is a soft preference only: the liked layouts
  have 15 to 18 of 57 links pointing backwards.

## Algorithms tried, in order, with results (graph above)

All sizes are the bounding box in px (nodes are 160 x 56).

| # | Algorithm | Result | Verdict |
|---|---|---|---|
| 1 | Layered, left to right (longest-path layers, stand-in nodes on long links, barycentre sweeps keeping the ordering with fewest crossings, rows pulled toward neighbours) | 4360 x 531, 25 crossings | too elongated (the graph has 16 layers) |
| 2 | Layered, folded into K bands (K chosen so the aspect is nearest 16:9; K=2) | 1910 x 892, mean link 607 px | better, but very long links where the bands join; kept as a candidate |
| 3 | Force-directed (Fruchterman-Reingold style), fitted to 16:9, 12 restarts keeping the shortest links / fewest backward links | 1920 x 1102, mean link 229, 6 backward | "definitely keeper" |
| 4 | Clustered force-directed: each node type pulled to a cell of a grid (cells ordered by mean layer) plus the force terms | 2034 x 1216, mean link 298, ~15 backward | "very good, just needs a pass to move nodes on top of edges" |
| 4b | + edge pass (nodes off links) | see below | |
| 4c | + tighter, then looser, spacing | see below | "too dense" at 18 px gaps; fine around 40 px |
| 4d | + edge-text rules | 2200 x 1261 (stretched) | text fits, no label covered |
| 4e | + compaction by annealing | 1491 x 1151, mean link 259, 18 backward | "very good, now just a few overlapping edge texts" |
| 4f | + unified search (all rules, one objective) | 1302 x 1183, mean link 273, zero defects; applied stretched 1.2x | current |

### Force-directed core (used by 3 and 4)

Per node `P[i]`, 700 iterations, cooling `t = 1 - it/iters`, step `min(|F|, (12 t + 0.5) * 8) * 0.25`:
- **Repulsion** between every pair, `k^2 / dist^2 * d` with an anisotropic distance
  `dist = sqrt(dx^2 + (1.6 dy)^2)` (boxes are wider than tall), summed and scaled by 0.6. `k` = 120 to 140.
- **Attraction** along each link, `dv * (L / k) * 0.9` (0.5 in the clustered variant).
- **Flow**: if a dependant is not at least `W + 70` right of its dependency, push them apart
  horizontally (0.6; in the clustered variant only for links within one type, 0.3).
- **Cluster pull** (clustered variant only): `(C_i - P_i) * 0.5 to 0.6 * (0.4 + t)` toward the cell
  centre of the node's type. Cells: types ordered by mean layer, `cols = round(sqrt(T * 16/9))`,
  cell size 360 x 253.
- Start: x = layer * 60, y uniform in -300..300 (clustered: cell centre +- 150).
- After the loop: stretch x (or y) so the bounding box is 16:9, then separate overlapping boxes
  (axis of least overlap, half each, up to 400 rounds, margin `M` between boxes).
- Restarts with different seeds; pick by `mean link length + 250 * backward links` (plus a
  penalty for aspect and size in the clustered variant).

### Edge pass: keep nodes off links

A link is the straight segment between the two box centres. Only the **core** of a node counts as
an obstacle: the box shrunk by a third of its width/height on every side (`CX = W/6 - W/2`,
`CY = H/6 - H/2` added to the half sizes), so a link may cut a third into a node.
- A plain nudge (move the node along the link's normal) reduced 16 offenders to 5 and made
  links longer (298 to 350 px mean).
- A greedy local search (candidates in 24 directions x several distances, cost = 1000 per node on
  a link + 5000 per box overlap + link length + displacement) got to 3 offenders, then stuck.
- **Simulated annealing** over the offending nodes plus the nodes at the ends of the links they
  block (accept uphill with `exp(-delta / T)`, T 80 to 240, radii 30 to 360) reached zero. It wandered
  (size grew to 2500 x 2200), so temperature and a displacement penalty (weight 1.0 to 1.5) were
  tuned until it stayed near the start. Adding the end nodes of the blocked link to the pool was the
  step that unlocked the last offenders.
- Stretching x by 1.2 to 1.25 afterwards kept zero offenders and moved the aspect to about 1.7.

### Compaction

- A greedy pass (move a node a step toward the centroid when that creates no overlap and no new
  defect and shortens its links) squeezed only about 1%, because the label rule pins every linked
  pair at about 86 px.
- **Annealing with the objective** `1000 * defects + 0.5 * incident link length + 0.35 * distance to
  the centroid (y weighted 1.78)` on single-node moves (radii 15 to 160, T 60 falling to 0.5,
  12,000 steps, about a minute in Python) took 1805 x 1261 down to 1491 x 1151.

### Edge-text rules

- **Room**: visible link length (centre line clipped by both boxes) must be at least
  `NEED = label_width + 2 * ARROW_LEN + 8 + 6`; `label_width` is estimated as 5.8 px per character
  at 11 px. Enforced as a separation force in the layout, as a penalty in the searches, and as a
  `pair_ok` test (linked pairs need `NEED` of visible length; unlinked pairs need only the margin).
- **Not covered by a node**: the label sits at the link's midpoint, offset 8 px to its "up" side
  (as the client draws it); sample 7 x 3 points over its box and require none to fall inside any
  node box (+2 px).
- **Labels not overlapping each other**: oriented rectangles (centre, direction, half length
  `label_width / 2 + 3`, half height 7.5), separating-axis test.
- Results: the first version had 29 of 57 links with no room and 13 labels under nodes; the
  constrained search removed both. Label-label overlaps (10 in the "very good" layout) were not yet
  all removed.

## Solved: the combined search

`tools/layout-experiments/unified.py` runs one annealing over single-node moves with **one
objective**: `1000 * defects + 0.5 * link length + 0.35 * distance to the centroid (y weighted 1.78)`,
where a defect is any of: boxes closer than the margin (28 px; linked pairs also need `NEED` of
visible link), a link through a node core, a link too short for its text, text under a node, text over
text. Each move is a random step (10 to 260 px) of a node, drawn 85% of the time from the nodes
involved in a current defect; temperature falls from about 60 to 0.5 in cycles of 3000 steps and is
raised when no progress is made for a while (the "reheat" is what cleared the last defects); the best
state seen (fewest defects, then smallest area, mildly preferring 16:9) is kept. Eight seeds run in
parallel; ~9000 steps each.

Result on the test graph: starting from the 1491 x 1151 layout with 10 defects, 5 of 8 seeds reached
zero within 6000 steps; a second run from a zero-defect result shrank it to **1302 x 1183, mean link
273 px, zero defects** (checked: 0 overlaps, 0 cores, 0 short links, 0 covered texts, 0 overlapping
texts). Stretching x afterwards keeps zero defects up to at least 1.5x (checked 1.0 to 1.5 in steps of
0.1). The layout on the production graph is that result stretched 1.2x: 1530 x 1183, aspect 1.29,
mean link 306 px, 18 backward links (`tools/layout-experiments/synthline-layout.json`, top-left
positions by node key order).

Note on the aspect: on a 16:9 screen the height binds (about 1183 px tall needs about 0.8 zoom), so
stretching wider does not make it fit better, it only lengthens links. The stretch is a taste knob.

Why the earlier fix-up loops failed: each fixed one defect type and was blind to the others, so
they traded one for another. Counting all defects in one objective and reheating when stuck fixed it.

## Next steps

1. (Done in Python, see above.) The pipeline is: clustered force, then the unified search, then a stretch.
2. Port to Rust (`crates/layout/src/tidy.rs`): deterministic RNG (xorshift with fixed seeds), grid
   or spatial index for the pair tests (the Python is O(N^2 E)), a work budget that scales with N
   (the O(N^2) force step is fine to a few hundred nodes; fall back to the layered layout above ~800).
3. Measure the real label width in the product: longest label among edge types in use, 11 px font.
   The text-fit rule depends on it.
4. Add the `tidy` op: all positions overwritten, anchors rebuilt (`as_places`: each node anchored to
   its nearest dependency, others at the origin), optional `aspect` argument, MCP tool `tidy_graph`,
   a "tidy" button for editors.
5. Use it for a batch of 3 or more `create_node` ops without `x`/`y` (replacing the layered layout),
   and keep `layered` available.
6. Unit tests: no overlaps, aspect within a tolerance, deterministic output, the five rules above on
   a generated graph, and the cluster rule falling back to plain force-directed when there is one
   node type.

## Files

- `tools/layout-experiments/lay.py`: all experiments (layered, folded, force, clustered, edge
  passes, compaction, label rules, checks). `PYTHONPATH=. python3 run.py <name>` writes
  `<name>.json`. It reads `../backup-positions.json` (a `GET /full` dump) and applies positions
  with `apply()` (REST) or, better, `ydoc.cjs`.
- `tools/layout-experiments/pipe.py`: the pipeline of clustered force, edge anneal and compaction
  with ten seeds (`python3 pipe.py <gap> <k> <margin> <out.json>`).
- `tools/layout-experiments/ydoc.cjs`: connects to the shared document as an editor and writes
  every node's place (`dump`, `set <positions.json>`, `restore <dump>`). Needs `../key.txt` (the
  edit key, never commit it), `../g2.txt` (the graph's viewer key) and `../backup-positions.json`.
  Run with the client's `node_modules` (paths are hard-coded to `client/node_modules`).

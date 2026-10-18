import lay, json, math, sys
import numpy as np
from multiprocessing import Pool
W, H = lay.W, lay.H
N = lay.N; links = lay.links

def total_defects(out):
    P = [[x+W/2, y+H/2] for x, y in out]
    n = len(lay.violations(P)) + lay.check(out) + len(lay.short_edges(out)) + len(lay.labels_covered(out)) + len(lay.label_overlaps(out))
    # nodes closer than the margin without overlapping boxes also count
    for i in range(N):
        for j in range(i+1, N):
            if not lay.pair_ok(P[i], P[j], frozenset((i, j)) in lay.LINKSET, lay.M): n += 1
    return n

def run(args):
    start, seed, steps = args
    lay.M = 28
    rng = np.random.default_rng(seed)
    P = [[x+W/2, y+H/2] for x, y in start]
    inc = [[(f, t) for f, t in links if i in (f, t)] for i in range(N)]
    def defects(i, q):
        c = 0
        for j in range(N):
            if j != i and not lay.pair_ok(q, P[j], frozenset((i, j)) in lay.LINKSET, lay.M): c += 1
        for f, t in links:
            if i in (f, t): continue
            if lay.seg_hits(P[f], P[t], q[0], q[1], W/2+lay.CX, H/2+lay.CY): c += 1
        for f, t in inc[i]:
            a = q if f == i else P[f]; b = q if t == i else P[t]
            for j in range(N):
                if j not in (f, t) and lay.seg_hits(a, b, P[j][0], P[j][1], W/2+lay.CX, H/2+lay.CY): c += 1
        return c + lay.label_defects(P, i, q)
    def compactness(i, q, cx, cy):
        e = sum(math.hypot((q if f == i else P[f])[0]-(q if t == i else P[t])[0], (q if f == i else P[f])[1]-(q if t == i else P[t])[1]) for f, t in inc[i])
        return 0.5*e + 0.35*math.hypot(q[0]-cx, (q[1]-cy)*1.78)
    def obj(i, q, cx, cy): return 1000*defects(i, q) + compactness(i, q, cx, cy)
    def snapshot():
        return lay.to_topleft([(x, y) for x, y in P])
    def bad_nodes():
        q = [[p[0]-W/2, p[1]-H/2] for p in P]; Pc = P
        out = []
        for a, b in lay.label_overlaps(q): out += list(links[a])+list(links[b])
        for f, t in links:
            if lay.vis_pts(Pc[f], Pc[t]) < lay.NEED: out += [f, t]
        for f, t, j in lay.labels_covered(q): out += [f, t, j]
        for i, f, t in lay.violations(Pc): out += [i, f, t]
        for i in range(N):
            for j in range(i+1, N):
                if not lay.pair_ok(Pc[i], Pc[j], frozenset((i, j)) in lay.LINKSET, lay.M): out += [i, j]
        return out
    best = ((10**9, 0), None)
    bad = bad_nodes(); stuck = 0
    for step in range(steps):
        if step % 150 == 0:
            bad = bad_nodes()
            n = len(bad)
            snap = snapshot()
            xs = [p[0] for p in snap]; ys = [p[1] for p in snap]
            w = max(xs)-min(xs)+W; h = max(ys)-min(ys)+H
            key = (n, w*h*(1+0.5*abs(math.log(w/h/(16/9)))))
            if key < best[0]: best = (key, snap); stuck = 0
            else: stuck += 1
        cx = sum(p[0] for p in P)/N; cy = sum(p[1] for p in P)/N
        i = int(rng.choice(bad)) if bad and rng.random() < 0.85 else int(rng.integers(N))
        # reheat in cycles; hotter and bigger moves when stuck
        phase = (step % 3000)/3000
        T = ((60+40*min(stuck, 6)) if best[0][0] > 0 else 25)*(1-phase)+0.5
        r = rng.choice([10, 20, 40, 70, 110, 180] if stuck < 3 else [20, 40, 70, 110, 180, 260]); a = rng.uniform(0, 2*math.pi)
        q = [P[i][0]+r*math.cos(a), P[i][1]+r*math.sin(a)]
        d = obj(i, q, cx, cy)-obj(i, P[i], cx, cy)
        if d < 0 or rng.random() < math.exp(-d/T): P[i] = q
    out = best[1]
    return (total_defects(out), seed, out)

if __name__ == '__main__':
    src = sys.argv[1]; seeds = int(sys.argv[2]); steps = int(sys.argv[3]); outname = sys.argv[4]
    start = [tuple(p) for p in json.load(open(src))]
    print('start defects', total_defects(start), flush=True)
    with Pool(min(seeds, 8)) as pool:
        res = pool.map(run, [(start, s, steps) for s in range(1, seeds+1)])
    for d, s, out in sorted(res, key=lambda r: r[0]): print('seed', s, 'defects', d, flush=True)
    res.sort(key=lambda r: (r[0], 0))
    # among zero-defect results prefer the smallest area
    zero = [r for r in res if r[0] == 0]
    def score(r):
        w = max(p[0] for p in r[2])-min(p[0] for p in r[2])+160; h = max(p[1] for p in r[2])-min(p[1] for p in r[2])+56
        print('  seed', r[1], round(w), round(h), round(w/h, 2))
        return w*h*(1+0.5*abs(math.log(w/h/(16/9))))
    pick = min(zero, key=score) if zero else res[0]
    lay.stats(pick[2], 'picked:'); print('defects', pick[0])
    json.dump(pick[2], open(outname, 'w'))

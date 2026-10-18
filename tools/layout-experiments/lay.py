import json, math, sys, urllib.request, random
import numpy as np
W, H, M = 160, 56, 24
D = json.load(open('../backup-positions.json'))
nodes = sorted(D['nodes'], key=lambda n: n['key'])
ids = [n['ref'] for n in nodes]
ix = {r: i for i, r in enumerate(ids)}
links = [(ix[l['from']], ix[l['to']]) for l in D['links'] if l['from'] in ix and l['to'] in ix]  # from depends on to
N = len(ids)
types = [n.get('node_type') or 'default' for n in nodes]

def layers_of():
    layer = [0]*N
    for _ in range(N):
        ch = False
        for f, t in links:
            if layer[f] < layer[t]+1: layer[f] = layer[t]+1; ch = True
        if not ch: break
    return layer

def layered(pitch_main=W+120, gap_cross=M, flip=False, lanes=None):
    """Sugiyama-lite. Returns centre (main, cross) per node; main = layer axis."""
    layer = layers_of()
    items = [dict(l=layer[i], left=[], right=[], real=True) for i in range(N)]
    for f, t in links:
        prev = t
        for l in range(layer[t]+1, layer[f]):
            items.append(dict(l=l, left=[], right=[], real=False)); k = len(items)-1
            items[prev]['right'].append(k); items[k]['left'].append(prev); prev = k
        items[prev]['right'].append(f); items[f]['left'].append(prev)
    C = max(i['l'] for i in items)+1
    order = [[k for k, it in enumerate(items) if it['l'] == c] for c in range(C)]
    if lanes:  # keep lane order as the initial order
        for c in range(C): order[c].sort(key=lambda k: (lanes[k] if k < N else 99, k))
    def cross(order):
        tot = 0
        for c in range(C-1):
            rank = {k: r for r, k in enumerate(order[c+1])}
            segs = [(r, rank[j]) for r, k in enumerate(order[c]) for j in items[k]['right'] if j in rank]
            for a in range(len(segs)):
                for b in range(a+1, len(segs)):
                    if (segs[a][0]-segs[b][0])*(segs[a][1]-segs[b][1]) < 0: tot += 1
        return tot
    def reorder(c, ref, left):
        rank = {k: r for r, k in enumerate(order[ref])}
        keyed = []
        for own, k in enumerate(order[c]):
            nb = [rank[j] for j in (items[k]['left'] if left else items[k]['right']) if j in rank]
            keyed.append((sum(nb)/len(nb) if nb else own, own, k))
        keyed.sort(key=lambda x: (x[0], x[1]))
        order[c] = [k for *_, k in keyed]
    best, bc = [o[:] for o in order], cross(order)
    for s in range(24):
        if s % 2 == 0:
            for c in range(1, C): reorder(c, c-1, True)
        else:
            for c in range(C-2, -1, -1): reorder(c, c+1, False)
        x = cross(order)
        if x < bc: bc, best = x, [o[:] for o in order]
    order = best
    h = lambda k: (H if k < N else 0)
    y = [0.0]*len(items)
    for col in order:
        at = 0
        for k in col: y[k] = at+h(k)/2; at += h(k)+gap_cross
    for p in range(10):
        for c in (range(C) if p % 2 == 0 else range(C-1, -1, -1)):
            col = order[c]
            want = []
            for k in col:
                nb = [y[j] for j in items[k]['left']+items[k]['right']]
                want.append(sum(nb)/len(nb) if nb else y[k])
            gap = lambda a, b: (h(a)+h(b))/2+gap_cross
            pos = want[:]
            for i in range(1, len(col)): pos[i] = max(pos[i], pos[i-1]+gap(col[i-1], col[i]))
            sh = sum(p_-w for p_, w in zip(pos, want))/len(col)
            pos = [p_-sh for p_ in pos]
            for i in range(1, len(col)): pos[i] = max(pos[i], pos[i-1]+gap(col[i-1], col[i]))
            for i, k in enumerate(col): y[k] = pos[i]
    return [(layer[i]*pitch_main, y[i]) for i in range(N)], bc

def to_topleft(centres_xy):
    xs = [c[0] for c in centres_xy]; ys = [c[1] for c in centres_xy]
    ox = min(p['position']['x'] for p in nodes); oy = min(p['position']['y'] for p in nodes)
    return [(round(x-W/2-(min(xs)-W/2)+ox), round(y-H/2-(min(ys)-H/2)+oy)) for x, y in centres_xy]

def layered_lr():
    c, bc = layered(W+120)
    print('crossings', bc)
    return to_topleft([(a, b) for a, b in c])

def layered_tb():
    c, bc = layered(H+110, gap_cross=M+16)
    print('crossings', bc)
    # dependencies on top: main axis -> y, cross axis -> x. Cross spacing was computed with node height; widen it.
    return to_topleft([(b*(W+M+16)/(H+M+16), a) for a, b in c])

ALG = {'layered_lr': layered_lr, 'layered_tb': layered_tb}
def current(): return [(n['position']['x'], n['position']['y']) for n in nodes]

def check(pos):
    bad = 0
    for i in range(N):
        for j in range(i+1, N):
            if abs(pos[i][0]-pos[j][0]) < W and abs(pos[i][1]-pos[j][1]) < H: bad += 1
    return bad

def apply(pos, key, graph, peer='layout-exp'):
    ops = [{'op': 'move_node', 'node': ids[i], 'x': pos[i][0], 'y': pos[i][1]} for i in range(N)]
    req = urllib.request.Request(f'https://waypoint.qdat.net/api/graphs/{graph}/apply', data=json.dumps({'label': 'layout experiment', 'ops': ops}).encode(), method='POST',
        headers={'Authorization': 'Bearer '+key, 'Content-Type': 'application/json', 'X-Waypoint-Actor': 'layout-experiment', 'X-Waypoint-Peer': peer})
    return urllib.request.urlopen(req).status

def fetch(key, graph):
    req = urllib.request.Request(f'https://waypoint.qdat.net/api/graphs/{graph}/full', headers={'Authorization': 'Bearer '+key})
    d = json.load(urllib.request.urlopen(req))
    m = {n['ref']: (n['position']['x'], n['position']['y']) for n in d['nodes']}
    return [m[r] for r in ids]

if __name__ == '__main__':
    name, do = sys.argv[1], len(sys.argv) > 2 and sys.argv[2] == 'apply'
    pos = ALG[name]()
    print(name, 'overlaps:', check(pos))
    json.dump(pos, open(f'{name}.json', 'w'))
    if do:
        key = open('../key.txt').read().strip(); graph = open('../g2.txt').read().strip()
        print('status', apply(pos, key, graph))
        got = fetch(key, graph)
        off = sum(1 for a, b in zip(pos, got) if tuple(a) != tuple(b))
        print('nodes not where planned:', off)

def folded(target=16/9, pitch=W+90, band_gap=70):
    c, bc = layered(pitch)
    layer = layers_of(); C = max(layer)+1
    best = None
    for K in range(1, 7):
        per = math.ceil(C/K)
        pos = [None]*N; y0 = 0; hmax = []; 
        for b in range(K):
            mem = [i for i in range(N) if layer[i]//per == b]
            if not mem: continue
            top = min(c[i][1] for i in mem)-H/2; bot = max(c[i][1] for i in mem)+H/2
            for i in mem: pos[i] = ((layer[i]-b*per)*pitch, c[i][1]-top+y0)
            y0 += (bot-top)+band_gap
        xs = [p[0] for p in pos]; ys = [p[1] for p in pos]
        w = max(xs)-min(xs)+W; h = max(ys)-min(ys)+H
        score = abs(math.log((w/h)/target))
        print('K', K, 'size', round(w), 'x', round(h), 'aspect', round(w/h, 2))
        if best is None or score < best[0]: best = (score, K, pos)
    print('chose K', best[1])
    return to_topleft(best[2])
ALG['folded'] = folded

def force(seed=3, iters=700, target=16/9, k=230.0):
    rng = np.random.default_rng(seed)
    layer = np.array(layers_of(), float)
    # Start from the layered x so dependencies begin left of dependants, with random y.
    P = np.stack([layer*60, rng.uniform(-300, 300, N)], 1)
    E = np.array(links)

    for it in range(iters):
        t = (1-it/iters)
        d = P[:, None, :]-P[None, :, :]
        # anisotropic distance: boxes are wider than tall
        dist = np.sqrt((d[..., 0]/1.0)**2+(d[..., 1]*1.6)**2)+1e-6
        rep = (k*k/dist**2)[..., None]*d
        np.fill_diagonal(rep[..., 0], 0); np.fill_diagonal(rep[..., 1], 0)
        F = rep.sum(1)*0.6
        for f, to in E:
            dv = P[to]-P[f]; L = np.linalg.norm(dv)+1e-6
            a = (L*L/k)*0.02*dv/L*L/ k
            F[f] += dv*(L/k)*0.9; F[to] -= dv*(L/k)*0.9
            # flow: dependant (f) should sit right of dependency (to)
            gap = (P[f][0]-P[to][0])-(W+70)
            if gap < 0: F[f][0] -= gap*0.6; F[to][0] += gap*0.6
        F[:, 0] += (layer-layer.mean())*0.0  # no global drift
        step = 12*t+0.5
        norm = np.linalg.norm(F, axis=1, keepdims=True)+1e-6
        P += F/norm*np.minimum(norm, step*8)*0.25
    # fit the aspect ratio
    w = np.ptp(P[:, 0])+W; h = np.ptp(P[:, 1])+H
    if w/h < target: P[:, 0] *= (target*h/w)
    else: P[:, 1] *= (w/target/h)
    # remove overlaps
    for _ in range(400):
        moved = False
        for i in range(N):
            for j in range(i+1, N):
                dx = P[j][0]-P[i][0]; dy = P[j][1]-P[i][1]
                ox = W+M-abs(dx); oy = H+M-abs(dy)
                if ox > 0 and oy > 0:
                    moved = True
                    if ox/(W+M) < oy/(H+M):
                        s = ox/2*(1 if dx >= 0 else -1); P[i][0] -= s; P[j][0] += s
                    else:
                        s = oy/2*(1 if dy >= 0 else -1); P[i][1] -= s; P[j][1] += s
        if not moved: break
    w = np.ptp(P[:, 0])+W; h = np.ptp(P[:, 1])+H
    print('size', round(w), 'x', round(h), 'aspect', round(w/h, 2))
    return to_topleft([(float(x), float(y)) for x, y in P])
ALG['force'] = force

def edge_len(pos):
    return sum(math.hypot(pos[f][0]-pos[t][0], pos[f][1]-pos[t][1]) for f, t in links)/len(links)
def backwards(pos): return sum(1 for f, t in links if pos[f][0] < pos[t][0])

def clustered(seed=1, iters=700, target=16/9, k=120.0, pull=0.5):
    rng = np.random.default_rng(seed)
    layer = np.array(layers_of(), float)
    kinds = sorted(set(types), key=lambda t: np.mean([layer[i] for i in range(N) if types[i] == t]))
    T = len(kinds); cols = max(1, round(math.sqrt(T*target))); rows = math.ceil(T/cols)
    cell = np.array([360.0, 360.0/target*1.0*1.25])
    centre = {t: np.array([(i % cols)*cell[0], (i//cols)*cell[1]]) for i, t in enumerate(kinds)}
    C = np.array([centre[types[i]] for i in range(N)])
    P = C + rng.uniform(-150, 150, (N, 2))
    E = np.array(links)
    for it in range(iters):
        tt = 1-it/iters
        d = P[:, None, :]-P[None, :, :]
        dist = np.sqrt(d[..., 0]**2+(d[..., 1]*1.6)**2)+1e-6
        rep = (k*k/dist**2)[..., None]*d
        np.fill_diagonal(rep[..., 0], 0); np.fill_diagonal(rep[..., 1], 0)
        F = rep.sum(1)*0.6
        for f, to in E:
            dv = P[to]-P[f]; L = np.linalg.norm(dv)+1e-6
            F[f] += dv*(L/k)*0.5; F[to] -= dv*(L/k)*0.5
            gap = (P[f][0]-P[to][0])-(W+70) if types[f] == types[to] else 0
            if gap < 0: F[f][0] -= gap*0.3; F[to][0] += gap*0.3
        F += (C-P)*pull*(0.4+tt)
        norm = np.linalg.norm(F, axis=1, keepdims=True)+1e-6
        P += F/norm*np.minimum(norm, (12*tt+0.5)*8)*0.25
    w = np.ptp(P[:, 0])+W; h = np.ptp(P[:, 1])+H
    if w/h < target: P[:, 0] *= (target*h/w)
    for _ in range(400):
        moved = False
        for i in range(N):
            for j in range(i+1, N):
                dx = P[j][0]-P[i][0]; dy = P[j][1]-P[i][1]
                ox = W+M-abs(dx); oy = H+M-abs(dy)
                if ox > 0 and oy > 0:
                    moved = True
                    if ox/(W+M) < oy/(H+M):
                        s = ox/2*(1 if dx >= 0 else -1); P[i][0] -= s; P[j][0] += s
                    else:
                        s = oy/2*(1 if dy >= 0 else -1); P[i][1] -= s; P[j][1] += s
        if separate_links(P): moved = True
        if not moved: break
    w = np.ptp(P[:, 0])+W; h = np.ptp(P[:, 1])+H
    print('kinds', kinds, 'size', round(w), 'x', round(h), 'aspect', round(w/h, 2))
    return to_topleft([(float(x), float(y)) for x, y in P])
ALG['clustered'] = clustered

CLEAR = 18
CX = W/6-W/2   # an edge may cut a third into a node: only its core counts
CY = H/6-H/2
def seg_hits(a, b, cx, cy, hw, hh):
    """Does segment a-b cross the box centred (cx,cy) with half sizes hw,hh? (Liang-Barsky)"""
    x0, y0 = a[0]-cx, a[1]-cy; dx, dy = b[0]-a[0], b[1]-a[1]
    t0, t1 = 0.0, 1.0
    for p, q in ((-dx, x0+hw), (dx, hw-x0), (-dy, y0+hh), (dy, hh-y0)):
        if abs(p) < 1e-9:
            if q < 0: return False
        else:
            r = q/p
            if p < 0:
                if r > t1: return False
                t0 = max(t0, r)
            else:
                if r < t0: return False
                t1 = min(t1, r)
    return t0 <= t1

def violations(P):
    out = []
    for i in range(N):
        for f, t in links:
            if i in (f, t): continue
            if seg_hits(P[f], P[t], P[i][0], P[i][1], W/2+CX, H/2+CY): out.append((i, f, t))
    return out

def clear_edges(pos, rounds=60):
    """pos are top-left; work on centres."""
    P = [[x+W/2, y+H/2] for x, y in pos]
    for r in range(rounds):
        v = violations(P)
        if not v: break
        moved = {}
        for i, f, t in v:
            a, b = P[f], P[t]
            dx, dy = b[0]-a[0], b[1]-a[1]; L = math.hypot(dx, dy) or 1
            nx, ny = -dy/L, dx/L                    # unit normal
            side = (P[i][0]-a[0])*nx+(P[i][1]-a[1])*ny
            s = 1 if side >= 0 else -1
            # distance along the normal needed to clear the (inflated) box
            need = abs(nx)*(W/2+CLEAR)+abs(ny)*(H/2+CLEAR)-abs(side)
            if need > 0:
                m = moved.setdefault(i, [0.0, 0.0])
                step = (need+4)
                if abs(m[0]) + abs(m[1]) < step:
                    m[0] = nx*s*step; m[1] = ny*s*step
        for i, (mx, my) in moved.items(): P[i][0] += mx; P[i][1] += my
        # separate overlaps again
        for _ in range(200):
            any_ = False
            for i in range(N):
                for j in range(i+1, N):
                    dx = P[j][0]-P[i][0]; dy = P[j][1]-P[i][1]
                    ox = W+M-abs(dx); oy = H+M-abs(dy)
                    if ox > 0 and oy > 0:
                        any_ = True
                        if ox/(W+M) < oy/(H+M):
                            s = ox/2*(1 if dx >= 0 else -1); P[i][0] -= s; P[j][0] += s
                        else:
                            s = oy/2*(1 if dy >= 0 else -1); P[i][1] -= s; P[j][1] += s
            if not any_: break
    print('edge/node violations left:', len(violations(P)), 'after', r+1, 'rounds')
    return to_topleft([(x, y) for x, y in P])

def clear_edges2(pos, rounds=40):
    P = [[x+W/2, y+H/2] for x, y in pos]
    inc = [[(f, t) for f, t in links if i in (f, t)] for i in range(N)]
    def local_cost(i, q, base):
        c = 0.0
        for j in range(N):                       # overlaps
            if j != i and not pair_ok(q, P[j], frozenset((i, j)) in LINKSET): c += 5000
        for f, t in links:                       # edges across node i
            if i in (f, t): continue
            if seg_hits(P[f], P[t], q[0], q[1], W/2+CX, H/2+CY): c += 1000
        for f, t in inc[i]:                      # i's own edges across others, and their length
            a = q if f == i else P[f]; b = q if t == i else P[t]
            for j in range(N):
                if j in (f, t): continue
                if seg_hits(a, b, P[j][0], P[j][1], W/2+CX, H/2+CY): c += 1000
            c += 0.4*math.hypot(a[0]-b[0], a[1]-b[1])
        c += 0.6*math.hypot(q[0]-base[0], q[1]-base[1])
        return c
    for r in range(rounds):
        bad = set()
        for i, f, t in violations(P): bad |= {i, f, t}
        for f, t in links:
            for j in range(N):
                if j not in (f, t) and seg_hits(P[f], P[t], P[j][0], P[j][1], W/2+CX, H/2+CY): bad.add(j)
        if not bad: break
        improved = False
        for i in sorted(bad):
            base = P[i][:]
            best = (local_cost(i, base, base), base)
            for ang in range(0, 360, 10):
                for dist in (30, 60, 90, 130, 180, 240, 320, 420, 560):
                    q = [base[0]+dist*math.cos(math.radians(ang)), base[1]+dist*math.sin(math.radians(ang))]
                    cst = local_cost(i, q, base)
                    if cst < best[0]-1e-6: best = (cst, q)
            if best[1] != base: P[i] = best[1]; improved = True
        if not improved: break
    print('violations left:', len(violations(P)), 'rounds', r+1)
    return to_topleft([(x, y) for x, y in P])

def anneal_clear(pos, steps=8000, seed=5):
    rng = np.random.default_rng(seed)
    P = [[x+W/2, y+H/2] for x, y in pos]; orig = [p[:] for p in P]
    inc = [[(f, t) for f, t in links if i in (f, t)] for i in range(N)]
    nbr = [set() for _ in range(N)]
    for f, t in links: nbr[f].add(t); nbr[t].add(f)
    def lc(i, q):
        c = 0.0
        for j in range(N):
            if j != i and abs(q[0]-P[j][0]) < W+M and abs(q[1]-P[j][1]) < H+M: c += 5000
        for f, t in links:
            if i in (f, t): continue
            if seg_hits(P[f], P[t], q[0], q[1], W/2+CX, H/2+CY): c += 1000
        for f, t in inc[i]:
            a = q if f == i else P[f]; b = q if t == i else P[t]
            for j in range(N):
                if j not in (f, t) and seg_hits(a, b, P[j][0], P[j][1], W/2+CX, H/2+CY): c += 1000
            c += 0.4*math.hypot(a[0]-b[0], a[1]-b[1])
            if vis_pts(a, b) < NEED: c += 1000
        c += 1000*label_defects(P, i, q)
        return c + DISP*math.hypot(q[0]-orig[i][0], q[1]-orig[i][1])
    def bad_set():
        s = set()
        for i, f, t in violations(P): s |= {i, f, t}
        for f, t, j in labels_covered([[p[0]-W/2, p[1]-H/2] for p in P]): s |= {f, t, j}
        for f, t in links:
            if vis_pts(P[f], P[t]) < NEED: s |= {f, t}
        return s
    bad = bad_set()
    for step in range(steps):
        if step % 200 == 0:
            bad = bad_set()
            if not bad: break
        pool = list(bad)
        i = int(rng.choice(pool)) if rng.random() < 0.7 else int(rng.integers(N))
        temp = TEMP*(1-step/steps)+1
        r = rng.choice(RADII); a = rng.uniform(0, 2*math.pi)
        q = [P[i][0]+r*math.cos(a), P[i][1]+r*math.sin(a)]
        d = lc(i, q)-lc(i, P[i])
        if d < 0 or rng.random() < math.exp(-d/temp): P[i] = q
    left = violations(P)+[(f, t, 0) for f, t in links if vis_pts(P[f], P[t]) < NEED]+labels_covered([[p[0]-W/2, p[1]-H/2] for p in P])
    print('defects left:', len(left), 'steps', step+1)
    return to_topleft([(x, y) for x, y in P])

TEMP=80; DISP=1.5; RADII=[30,60,120,200]

def compact(pos, margin=18, rounds=80, seed=1):
    rng = np.random.default_rng(seed)
    P = [[x+W/2, y+H/2] for x, y in pos]
    inc = [[(f, t) for f, t in links if i in (f, t)] for i in range(N)]
    def defects(i, q):
        c = 0
        for f, t in links:
            if i in (f, t): continue
            if seg_hits(P[f], P[t], q[0], q[1], W/2+CX, H/2+CY): c += 1
        for f, t in inc[i]:
            a = q if f == i else P[f]; b = q if t == i else P[t]
            for j in range(N):
                if j not in (f, t) and seg_hits(a, b, P[j][0], P[j][1], W/2+CX, H/2+CY): c += 1
        return c + label_defects(P, i, q)
    def free(i, q):
        return all(j == i or pair_ok(q, P[j], frozenset((i, j)) in LINKSET, margin) for j in range(N))
    def elen(i, q):
        return sum(math.hypot((q if f == i else P[f])[0]-(q if t == i else P[t])[0], (q if f == i else P[f])[1]-(q if t == i else P[t])[1]) for f, t in inc[i])
    for r in range(rounds):
        moved = 0
        cx = sum(p[0] for p in P)/N; cy = sum(p[1] for p in P)/N
        for i in rng.permutation(N):
            i = int(i); base = P[i][:]; d0 = defects(i, base); e0 = elen(i, base)
            best = None
            for dx, dy in ((-24, 0), (24, 0), (0, -16), (0, 16), (-24, -16), (24, 16), (-24, 16), (24, -16), (-12, 0), (12, 0), (0, -8), (0, 8)):
                q = [base[0]+dx, base[1]+dy]
                if not free(i, q): continue
                if defects(i, q) > d0: continue
                gain = (e0-elen(i, q)) + 0.15*(math.hypot(base[0]-cx, (base[1]-cy)*1.78)-math.hypot(q[0]-cx, (q[1]-cy)*1.78))
                if gain > 0.5 and (best is None or gain > best[0]): best = (gain, q)
            if best: P[i] = best[1]; moved += 1
        if not moved: break
    print('compact rounds', r+1)
    return to_topleft([(x, y) for x, y in P])

def stats(out, label=''):
    w = max(p[0] for p in out)-min(p[0] for p in out)+W; h = max(p[1] for p in out)-min(p[1] for p in out)+H
    P = [[x+W/2, y+H/2] for x, y in out]
    print(label, 'size', round(w), 'x', round(h), 'aspect', round(w/h, 2), 'edge', round(edge_len(out)), 'back', backwards(out), 'core-violations', len(violations(P)), 'overlaps', check(out))

LABEL_W = 9*5.8   # "data from", 11px system-ui
NEED = LABEL_W+2*10+8+6

def box_exit(c, t):
    dx, dy = t[0]-c[0], t[1]-c[1]
    if dx == 0 and dy == 0: return c
    k = min(math.inf if dx == 0 else (W/2)/abs(dx), math.inf if dy == 0 else (H/2)/abs(dy))
    return (c[0]+dx*k, c[1]+dy*k)

def visible(P, f, t):
    a = box_exit(P[f], P[t]); b = box_exit(P[t], P[f])
    # if the boxes overlap/touch the exits cross: treat as zero
    d = math.hypot(b[0]-a[0], b[1]-a[1])
    return d if (b[0]-a[0])*(P[t][0]-P[f][0]) + (b[1]-a[1])*(P[t][1]-P[f][1]) >= 0 else 0

def short_edges(out):
    P = [[x+W/2, y+H/2] for x, y in out]
    return [(f, t, visible(P, f, t)) for f, t in links if visible(P, f, t) < NEED]


LINKSET = {frozenset(l) for l in links}

def vis_pts(a, b):
    ea = box_exit(a, b); eb = box_exit(b, a)
    if (eb[0]-ea[0])*(b[0]-a[0])+(eb[1]-ea[1])*(b[1]-a[1]) < 0: return 0.0
    return math.hypot(eb[0]-ea[0], eb[1]-ea[1])

def pair_ok(a, b, linked, margin=None):
    m = M if margin is None else margin
    if abs(a[0]-b[0]) < W+m and abs(a[1]-b[1]) < H+m: return False
    return (not linked) or vis_pts(a, b) >= NEED

def separate_links(P):
    """Push linked pairs apart along their line until the label fits between the boxes. True if anything moved."""
    moved = False
    for f, t in links:
        v = vis_pts(P[f], P[t])
        if v < NEED:
            dx, dy = P[t][0]-P[f][0], P[t][1]-P[f][1]; L = math.hypot(dx, dy) or 1
            push = (NEED-v)/2+1
            P[f][0] -= dx/L*push; P[f][1] -= dy/L*push; P[t][0] += dx/L*push; P[t][1] += dy/L*push
            moved = True
    return moved


def label_samples(a, b):
    """Points covering the edge label's text box, for an edge between node centres a and b."""
    ea = box_exit(a, b); eb = box_exit(b, a)
    mid = ((ea[0]+eb[0])/2, (ea[1]+eb[1])/2)
    dx, dy = eb[0]-ea[0], eb[1]-ea[1]; L = math.hypot(dx, dy) or 1
    dx, dy = dx/L, dy/L
    if dx < 0 or (abs(dx) < 1e-9 and dy < 0): dx, dy = -dx, -dy       # text reads left to right
    nx, ny = dy, -dx                                                    # text's up side
    cx, cy = mid[0]+nx*8, mid[1]+ny*8
    pts = []
    for s in (-1, -0.66, -0.33, 0, 0.33, 0.66, 1):
        for h in (-6.5, 0, 6.5):
            pts.append((cx+dx*s*LABEL_W/2+nx*h, cy+dy*s*LABEL_W/2+ny*h))
    return pts

def covered(pts, c, pad=2):
    return any(abs(x-c[0]) < W/2+pad and abs(y-c[1]) < H/2+pad for x, y in pts)

def label_defects(P, i, q):
    """Edge labels that node i, standing at q, would cover, plus labels of i's own edges that others cover."""
    n = 0
    for f, t in links:
        if i in (f, t):
            a = q if f == i else P[f]; b = q if t == i else P[t]
            pts = label_samples(a, b)
            n += sum(1 for j in range(N) if j not in (f, t) and covered(pts, P[j]))
            n += 0
        else:
            if covered(label_samples(P[f], P[t]), q): n += 1
    return n

def labels_covered(out):
    P = [[x+W/2, y+H/2] for x, y in out]
    bad = []
    for f, t in links:
        pts = label_samples(P[f], P[t])
        for j in range(N):
            if j not in (f, t) and covered(pts, P[j], 0): bad.append((f, t, j))
    return bad

def anneal_compact(pos, steps=20000, seed=1, margin=28, wc=0.35, we=0.5, t0=60.0):
    rng = np.random.default_rng(seed)
    P = [[x+W/2, y+H/2] for x, y in pos]
    inc = [[(f, t) for f, t in links if i in (f, t)] for i in range(N)]
    def defects(i, q):
        c = 0
        for j in range(N):
            if j != i and not pair_ok(q, P[j], frozenset((i, j)) in LINKSET, margin): c += 5
        for f, t in links:
            if i in (f, t): continue
            if seg_hits(P[f], P[t], q[0], q[1], W/2+CX, H/2+CY): c += 1
        for f, t in inc[i]:
            a = q if f == i else P[f]; b = q if t == i else P[t]
            for j in range(N):
                if j not in (f, t) and seg_hits(a, b, P[j][0], P[j][1], W/2+CX, H/2+CY): c += 1
        return c + label_defects(P, i, q)
    def obj(i, q, cx, cy):
        e = sum(math.hypot((q if f == i else P[f])[0]-(q if t == i else P[t])[0], (q if f == i else P[f])[1]-(q if t == i else P[t])[1]) for f, t in inc[i])
        return 1000*defects(i, q) + we*e + wc*math.hypot(q[0]-cx, (q[1]-cy)*1.78)
    for step in range(steps):
        cx = sum(p[0] for p in P)/N; cy = sum(p[1] for p in P)/N
        i = int(rng.integers(N))
        T = t0*(1-step/steps)+0.5
        r = rng.choice([15, 30, 60, 100, 160]); a = rng.uniform(0, 2*math.pi)
        q = [P[i][0]+r*math.cos(a), P[i][1]+r*math.sin(a)]
        d = obj(i, q, cx, cy)-obj(i, P[i], cx, cy)
        if d < 0 or rng.random() < math.exp(-d/T): P[i] = q
    return to_topleft([(x, y) for x, y in P])

def label_rect(a, b):
    ea = box_exit(a, b); eb = box_exit(b, a)
    mid = ((ea[0]+eb[0])/2, (ea[1]+eb[1])/2)
    dx, dy = eb[0]-ea[0], eb[1]-ea[1]; L = math.hypot(dx, dy) or 1
    dx, dy = dx/L, dy/L
    if dx < 0 or (abs(dx) < 1e-9 and dy < 0): dx, dy = -dx, -dy
    nx, ny = dy, -dx
    return (mid[0]+nx*8, mid[1]+ny*8, dx, dy, LABEL_W/2+3, 7.5)

def rects_overlap(r1, r2):
    c1 = (r1[0], r1[1]); c2 = (r2[0], r2[1])
    for (_, _, dx, dy, _, _) in (r1, r2):
        for ax, ay in ((dx, dy), (-dy, dx)):
            def proj(r):
                _, _, rdx, rdy, hl, hh = r
                return hl*abs(ax*rdx+ay*rdy)+hh*abs(ax*(-rdy)+ay*rdx)
            if abs((c2[0]-c1[0])*ax+(c2[1]-c1[1])*ay) > proj(r1)+proj(r2): return False
    return True

def label_overlaps(out):
    P = [[x+W/2, y+H/2] for x, y in out]
    R = [label_rect(P[f], P[t]) for f, t in links]
    return [(a, b) for a in range(len(links)) for b in range(a+1, len(links)) if rects_overlap(R[a], R[b])]

_old_label_defects = label_defects
def label_defects(P, i, q):
    n = _old_label_defects(P, i, q)
    mine = [(k, label_rect(q if f == i else P[f], q if t == i else P[t])) for k, (f, t) in enumerate(links) if i in (f, t)]
    mine_ix = {k for k, _ in mine}
    for k, r in mine:
        for k2, (f2, t2) in enumerate(links):
            if k2 == k or (k2 in mine_ix and k2 < k): continue
            if rects_overlap(r, label_rect(P[f2], P[t2]) if k2 not in mine_ix else dict(mine)[k2]): n += 1
    return n

def fix_labels(pos, steps=6000, seed=1, margin=28, wc=0.35, we=0.5, t0=40.0):
    rng = np.random.default_rng(seed)
    P = [[x+W/2, y+H/2] for x, y in pos]
    inc = [[(f, t) for f, t in links if i in (f, t)] for i in range(N)]
    def defects(i, q):
        c = 0
        for j in range(N):
            if j != i and not pair_ok(q, P[j], frozenset((i, j)) in LINKSET, margin): c += 5
        for f, t in links:
            if i in (f, t): continue
            if seg_hits(P[f], P[t], q[0], q[1], W/2+CX, H/2+CY): c += 1
        for f, t in inc[i]:
            a = q if f == i else P[f]; b = q if t == i else P[t]
            for j in range(N):
                if j not in (f, t) and seg_hits(a, b, P[j][0], P[j][1], W/2+CX, H/2+CY): c += 1
        return c + label_defects(P, i, q)
    def obj(i, q, cx, cy):
        e = sum(math.hypot((q if f == i else P[f])[0]-(q if t == i else P[t])[0], (q if f == i else P[f])[1]-(q if t == i else P[t])[1]) for f, t in inc[i])
        return 1000*defects(i, q) + we*e + wc*math.hypot(q[0]-cx, (q[1]-cy)*1.78)
    def bad_nodes():
        out = []
        q = [[p[0]-W/2, p[1]-H/2] for p in P]
        for a, b in label_overlaps(q): out += list(links[a])+list(links[b])
        for f, t in links:
            if vis_pts(P[f], P[t]) < NEED: out += [f, t]
        for f, t, j in labels_covered(q): out += [f, t, j]
        return out
    bad = bad_nodes()
    for step in range(steps):
        if step % 100 == 0:
            bad = bad_nodes()
            if not bad: break
        cx = sum(p[0] for p in P)/N; cy = sum(p[1] for p in P)/N
        i = int(rng.choice(bad)) if bad and rng.random() < 0.8 else int(rng.integers(N))
        T = t0*(1-step/steps)+0.5
        r = rng.choice([10, 20, 40, 70, 110]); a = rng.uniform(0, 2*math.pi)
        q = [P[i][0]+r*math.cos(a), P[i][1]+r*math.sin(a)]
        d = obj(i, q, cx, cy)-obj(i, P[i], cx, cy)
        if d < 0 or rng.random() < math.exp(-d/T): P[i] = q
    out = to_topleft([(x, y) for x, y in P])
    print('label overlaps left', len(label_overlaps(out)), 'steps', step+1)
    return out

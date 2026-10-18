import lay, json, sys
lay.M = int(sys.argv[1]); K = float(sys.argv[2]); MARGIN = int(sys.argv[3]); out_name = sys.argv[4]
lay.TEMP = 120; lay.DISP = 1.2; lay.RADII = [30, 60, 120, 200]
res = []
for seed in range(1, 11):
    out = lay.clustered(seed=seed, k=K, pull=0.6)
    out = lay.anneal_clear(out, seed=seed, steps=4000)
    out = lay.compact(out, margin=MARGIN, seed=seed)
    P = [[x+80, y+28] for x, y in out]
    w = max(p[0] for p in out)-min(p[0] for p in out)+160; h = max(p[1] for p in out)-min(p[1] for p in out)+56
    v = len(lay.violations(P)); ov = lay.check(out); sh = len(lay.short_edges(out)); lc = len(lay.labels_covered(out))
    print(seed, round(w), round(h), round(w/h, 2), 'edge', round(lay.edge_len(out)), 'back', lay.backwards(out), 'core', v, 'overlap', ov, 'short', sh, 'covered', lc, flush=True)
    res.append(((v+ov)*1000+sh*100+lc*100+abs(w/h-16/9)*500+lay.edge_len(out)*0.5+w*h/5000, seed, out))
res.sort(key=lambda r: r[0]); print('best', res[0][1]); lay.stats(res[0][2], 'chosen:')
print('short', len(lay.short_edges(res[0][2])), 'covered', len(lay.labels_covered(res[0][2])))
json.dump(res[0][2], open(out_name, 'w'))

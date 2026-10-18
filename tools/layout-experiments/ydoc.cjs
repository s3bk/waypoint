const nm = '/home/sebk/Rust/waypoint/client/node_modules/';
const Y = require(nm + 'yjs');
const WS = require(nm + 'ws');
const { WebsocketProvider } = require(nm + 'y-websocket');
const fs = require('fs');
const [mode, file] = process.argv.slice(2);
const key = fs.readFileSync('../key.txt', 'utf8').trim();
const graph = fs.readFileSync('../g2.txt', 'utf8').trim();
const full = JSON.parse(fs.readFileSync('../backup-positions.json', 'utf8'));
const idOf = Object.fromEntries(full.nodes.map((n) => [n.ref, n.id]));
const refs = full.nodes.slice().sort((a, b) => a.key - b.key).map((n) => n.ref);
const doc = new Y.Doc();
const provider = new WebsocketProvider('wss://waypoint.qdat.net/ws', graph, doc, { WebSocketPolyfill: WS, params: { key } });
provider.on('sync', (ok) => {
  if (!ok) return;
  const nodes = doc.getMap('nodes');
  if (mode === 'dump') {
    const out = {};
    for (const [id, m] of nodes.entries()) out[id] = m.get('place');
    fs.writeFileSync(file, JSON.stringify(out));
    console.log('saved places of', Object.keys(out).length, 'nodes');
    process.exit(0);
  } else if (mode === 'restore') {
    const raw = JSON.parse(fs.readFileSync(file, 'utf8'));
    doc.transact(() => { for (const [id, p] of Object.entries(raw)) nodes.get(id)?.set('place', p); });
  } else if (mode === 'set') {
    const pos = JSON.parse(fs.readFileSync(file, 'utf8'));
    doc.transact(() => {
      refs.forEach((r, i) => nodes.get(idOf[r])?.set('place', JSON.stringify({ anchor: null, off: { x: pos[i][0], y: pos[i][1] } })));
    });
  }
  setTimeout(() => { console.log(mode, 'done'); process.exit(0); }, 1500);
});
setTimeout(() => { console.log('timeout'); process.exit(1); }, 20000);

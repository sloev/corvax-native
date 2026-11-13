// The JS half of the hosts, driven by a simulated native side (Node sockets standing in for Rust / Kotlin):
// the whole CORVAX node runs through the bridge, and a normal CORVAX app uses it as its local node.
//   setup: ./fetch-corvax.sh (or copy CORVAX's dist/, src/, assets/ into shared/corvax/)   run: node --test shared/test
import test from 'node:test'
import assert from 'node:assert/strict'
import { createSocket } from 'node:dgram'
import { createServer } from 'node:http'
import { readFile } from 'node:fs/promises'
import { startNode } from '../host-bridge.js'

const b64 = b => Buffer.from(b).toString('base64'), unb64 = s => new Uint8Array(Buffer.from(s, 'base64'))

// What desktop/src-tauri/src/main.rs and android/…/HostBridge.kt do, in Node.
function simulatedNative () {
  const handlers = new Map(), emit = (e, p) => queueMicrotask(() => (handlers.get(e) || []).forEach(fn => fn(p)))
  const udp = new Map(), pending = new Map(), servers = new Map(), sockets = new Map(), kv = new Map()
  let next = 0
  const calls = {
    udp_bind: ({ port, address, reuse, group, broadcast }) => new Promise((resolve, reject) => {
      const s = createSocket({ type: 'udp4', reuseAddr: reuse }), id = ++next
      s.on('message', (b, r) => emit('udp', { id, data: b64(b), address: r.address, port: r.port }))
      s.once('error', reject)
      s.bind(port, address, () => { try { if (broadcast) s.setBroadcast(true); if (group) s.addMembership(group) } catch {} udp.set(id, s); resolve({ id, port: s.address().port }) })
    }),
    udp_send: ({ id, data, port, host }) => { udp.get(id)?.send(unb64(data), port, host) },
    udp_close: ({ id }) => { udp.get(id)?.close(); udp.delete(id) },
    http_listen: ({ port, address }) => new Promise(resolve => {
      const sid = ++next
      const server = createServer((req, res) => {
        const parts = []
        req.on('data', d => parts.push(d)).on('end', () => {
          const rid = ++next
          pending.set(rid, out => { res.writeHead(out.status, out.headers); res.end(unb64(out.body)) })
          res.on('close', () => { if (pending.delete(rid)) emit('http-abort', { rid }) })
          emit('http', { sid, rid, method: req.method, path: new URL(req.url, 'http://x').pathname, headers: req.headers,
            ip: req.socket.remoteAddress.replace(/^::ffff:/, ''), port: req.socket.remotePort, body: b64(Buffer.concat(parts)) })
        })
      })
      servers.set(sid, server)
      server.listen(port, address, () => resolve({ sid }))
    }),
    http_respond: ({ rid, status, headers, body }) => { const f = pending.get(rid); pending.delete(rid); f?.({ status, headers, body }) },
    http_close: ({ sid }) => new Promise(resolve => { const s = servers.get(sid); s.closeAllConnections(); s.close(resolve) }),
    ws_send () {}, ws_close () {},
    kv_get: ({ name }) => kv.get(name) ?? null,
    kv_set: ({ name, value }) => { kv.set(name, value) },
    dns_txt: () => '',
    node_ready: () => {}
  }
  return {
    call: async (name, args) => calls[name](args),
    on: (e, fn) => handlers.set(e, [...(handlers.get(e) || []), fn]),
    close: () => { for (const s of udp.values()) s.close(); for (const s of servers.values()) s.closeAllConnections() }
  }
}

test('a node runs through the host bridge; an app on this device uses it as its local node', async t => {
  const bridge = simulatedNative()
  const asset = name => readFile(new URL('../corvax/' + name, import.meta.url)).then(b => new Uint8Array(b))
  const node = await startNode(bridge, { port: 0, host: '127.0.0.1', anchor: 'off', lan: false, fallback: [] }, { asset })
  const local = `http://127.0.0.1:${node.port}`
  t.after(async () => { await node.close(); bridge.close() })

  assert.match(await (await fetch(local + '/cx/app.js')).text(), /DOX/, 'homepage files come through the host')
  const { Peer } = await import('../corvax/src/index.js')
  const a = await Peer.create({ local, fallback: [] }), b = await Peer.create({ local, fallback: [] })
  t.after(async () => { await a.close(); await b.close() })
  assert.equal(a.inboxes[0].url, local)
  await b.publish()
  const got = new Promise(resolve => b.on(({ body }) => resolve(body)))
  await a.send(b.address, { hello: 'through the bridge' })              // a long poll held open across the bridge
  assert.deepEqual(await got, { hello: 'through the bridge' })
})

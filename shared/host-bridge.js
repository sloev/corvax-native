// The JS half of every native host: turns a platform bridge into CORVAX's host interface (SPEC §11).
// A platform provides `bridge` with:
//   call(name, args) → Promise<result>            (Tauri: invoke · Android: a @JavascriptInterface, answered via emit)
//   on(event, fn)                                  native → JS events: udp, http, http-abort, ws-open, ws-msg, ws-close, radio
// Bytes cross the bridge as base64.
const b64 = u8 => { let s = ''; for (let i = 0; i < u8.length; i += 8192) s += String.fromCharCode(...u8.subarray(i, i + 8192)); return btoa(s) }
const unb64 = s => Uint8Array.from(atob(s), c => c.charCodeAt(0))

export function makeHost (bridge, { assetBase = new URL('./corvax/', import.meta.url).href, asset } = {}) {
  const udps = new Map(), servers = new Map(), sockets = new Map(), aborts = new Map()

  bridge.on('udp', ({ id, data, address, port }) => udps.get(id)?.(unb64(data), { address, port }))
  bridge.on('http', async ({ sid, rid, method, path, headers, ip, port, body }) => {
    const ac = new AbortController()
    aborts.set(rid, ac)
    const res = await servers.get(sid)?.handle({
      method, path, headers, ip, port, signal: ac.signal,
      body: async max => { const b = unb64(body || ''); if (b.length > max) throw new Error('too large'); return b }
    }).catch(() => ({ status: 500 }))
    aborts.delete(rid)
    const out = typeof res.body === 'string' ? new TextEncoder().encode(res.body) : res.body || new Uint8Array()
    bridge.call('http_respond', { rid, status: res.status, headers: res.headers || {}, body: b64(out) })
  })
  bridge.on('http-abort', ({ rid }) => aborts.get(rid)?.abort())
  bridge.on('ws-open', ({ sid, wid, path }) => {
    const ws = {
      send: d => bridge.call('ws_send', { wid, text: typeof d === 'string' ? d : null, data: typeof d === 'string' ? null : b64(d) }),
      close: () => bridge.call('ws_close', { wid }), onmessage () {}, onclose () {}
    }
    sockets.set(wid, ws)
    servers.get(sid)?.onsocket(path, ws)
  })
  bridge.on('ws-msg', ({ wid, text, data }) => sockets.get(wid)?.onmessage(text ?? unb64(data)))
  bridge.on('ws-close', ({ wid }) => { sockets.get(wid)?.onclose(); sockets.delete(wid) })

  return {
    async udp ({ port = 0, address = '0.0.0.0', reuse = false, group, broadcast = false }, onmessage) {
      const { id, port: bound } = await bridge.call('udp_bind', { port, address, reuse, group: group || null, broadcast })
      udps.set(id, onmessage)
      return {
        port: bound,
        send: (bytes, p, host) => { bridge.call('udp_send', { id, data: b64(bytes), port: p, host }).catch(() => {}) },   // host may be "ble:<id>" (Android)
        close: () => { udps.delete(id); bridge.call('udp_close', { id }).catch(() => {}) }
      }
    },
    async http ({ port, address = '0.0.0.0' }, handle, onsocket) {
      const { sid } = await bridge.call('http_listen', { port, address })
      servers.set(sid, { handle, onsocket })
      return { close: () => { servers.delete(sid); return bridge.call('http_close', { sid }) } }
    },
    asset: asset || (name => fetch(assetBase + name).then(r => r.arrayBuffer()).then(b => new Uint8Array(b))),
    kv: () => ({ get: name => bridge.call('kv_get', { name }), set: (name, value) => bridge.call('kv_set', { name, value }) }),
    dnsTxt: name => bridge.call('dns_txt', { name })
  }
}

// Start a node with this host. Options are CORVAX node options (SPEC, README "Run a node").
export async function startNode (bridge, options = {}, hostOptions = {}) {
  const { start } = await import('./corvax/corvax-node.min.js')
  const io = makeHost(bridge, hostOptions)
  const node = await start({ io, port: 4799, host: '0.0.0.0', ...options })
  bridge.on('radio', ({ text }) => node.book.add(text, { lan: true }))               // anchor lines found over BLE / Wi-Fi Direct
  bridge.on('policy', ({ relay, storage }) => { if (node.peer) node.peer.relayFor = relay; node.o.storagePaused = !storage })
  bridge.call('node_ready', { key: node.key, line: node.line() || '', port: node.port }).catch(() => {})
  return node
}

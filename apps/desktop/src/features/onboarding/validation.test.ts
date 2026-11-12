import { describe, expect, it } from 'vitest'
import ssNet from '../../../../../crates/ss-net/src/lib.rs?raw'
import { parseAddress } from './validation'

// The server's QUIC and HTTPS default, read from the Rust source so the two
// cannot drift apart.
const serverDefault = Number(ssNet.match(/pub const DEFAULT_PORT: u16 = (\d+);/)?.[1])

describe('default port', () => {
  it('is ss_net::DEFAULT_PORT for QUIC and HTTPS when the address has none', () => {
    expect(serverDefault).toBeGreaterThan(0)
    expect(parseAddress('voice.example.com')).toEqual({
      host: 'voice.example.com',
      port: serverDefault,
      httpsPort: serverDefault,
    })
  })
  it('keeps a typed port, as saved addresses have', () => {
    expect(parseAddress('host:5781')).toEqual({ host: 'host', port: 5781, httpsPort: 5781 })
  })
})

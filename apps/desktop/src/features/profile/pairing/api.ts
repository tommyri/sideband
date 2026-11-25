import { listen } from '@tauri-apps/api/event'
import { invokeBridge } from '../../../lib/tauri'
import type { PairAction } from '../../../lib/generated/PairAction'
import type { PairView } from '../../../lib/generated/PairView'
import type { ProbeInput } from '../../../lib/bindings'
import { parseAddress } from '../../onboarding/validation'

export const startPairing = (id: string) => invokeBridge<PairView>('pair_start', { id })
export const joinPairing = (id: string, target: ProbeInput, code: string, name: string) =>
  invokeBridge<PairView>('pair_join', { id, target, code, name })
export const updatePairing = (id: string, action: PairAction) =>
  invokeBridge<PairView>('pair_update', { id, action })

export function relayAddress(target: ProbeInput) {
  const host = target.host.includes(':') ? '[' + target.host + ']' : target.host
  return (
    host +
    ':' +
    target.port +
    (target.port === target.httpsPort ? '' : '?https=' + target.httpsPort)
  )
}
export function pairingLink(target: ProbeInput, code: string) {
  const url = new URL('sideband://pair')
  url.searchParams.set('relay', relayAddress(target))
  url.hash = code
  return url.toString()
}
export function readInvitation(input: string, manualCode: string) {
  let address = input.trim(),
    code = manualCode.trim()
  if (address.toLowerCase().startsWith('sideband:')) {
    const link = new URL(address)
    if (
      link.protocol !== 'sideband:' ||
      link.host !== 'pair' ||
      link.username ||
      link.password ||
      link.pathname ||
      !link.hash
    )
      throw new Error('Use the pairing link shown on your trusted computer.')
    address = link.searchParams.get('relay') ?? ''
    code = link.hash.slice(1)
  }
  const [host, query, ...extra] = address.split('?')
  if (extra.length || (query !== undefined && !/^https=\d+$/.test(query)))
    throw new Error('Copy the full sync server address from the trusted computer.')
  return { target: parseAddress(host ?? '', query?.slice(6)), code }
}
export async function listenForPairing(received: (link: string) => void) {
  let openedWhileStarting = false
  const accept = (value: string) => {
    if (!value.startsWith('sideband://pair?')) return
    try {
      readInvitation(value, '')
    } catch {
      return
    }
    openedWhileStarting = true
    received(value)
  }
  const stop = await listen<string[]>('deep-link://new-url', (event) => {
    for (const url of event.payload) accept(url)
  })
  try {
    const current = await invokeBridge<string | null>('current_pair_link')
    if (current && !openedWhileStarting) accept(current)
  } catch {
    /* The live listener still accepts links after startup. */
  }
  return stop
}

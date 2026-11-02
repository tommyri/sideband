import type { ApiError, ProbeInput } from '../../lib/bindings'

export function parseAddress(value: string, httpsPort?: string): ProbeInput {
  const raw = value.trim().replace(/^ss:\/\//i, '')
  if (!raw || /[\s/@?#]/.test(raw) || raw.includes('://'))
    throw new Error('Enter a host, host:port, or ss://host:port.')
  const match = raw.match(/^(\[[a-f\d:]+\]|[^:]+)(?::(\d+))?$/i)
  if (!match || !match[1]) throw new Error('Use [IPv6]:port for an IPv6 address.')
  const port = Number(match[2] ?? 5781)
  const https = httpsPort ? Number(httpsPort) : port
  if (![port, https].every((p) => Number.isInteger(p) && p > 0 && p <= 65535))
    throw new Error('Ports must be between 1 and 65535.')
  return { host: match[1].replace(/^\[|\]$/g, '').toLowerCase(), port, httpsPort: https }
}
export function authError(username: string, password: string, register: boolean): string | null {
  if (!username.trim() || !password) return 'Enter your username and password.'
  if (register && !/^[a-z0-9_.]{3,32}$/.test(username))
    return 'Use 3–32 lowercase letters, digits, underscores, or dots for your username.'
  const length = new TextEncoder().encode(password).length
  if (length > 128 || (register && length < 8)) return 'Use a password between 8 and 128 bytes.'
  return null
}
const errors: Record<string, string> = {
  server_1: 'Your login or saved session was not accepted. Check your username and password.',
  server_2: 'Registration is unavailable or this invite is not allowed. Ask the server owner.',
  server_3: 'That invite or account could not be found.',
  server_4: 'Check your username, password, and invite code.',
  server_5: 'Too many attempts. Wait a moment before trying again.',
  server_6: 'That username is already taken. Choose another or log in.',
  server_7: 'This account is banned from the server. Contact its owner.',
  server_8: 'The server encountered a problem. Try again later.',
  server_9: 'This app and server use different protocol versions. Update the older one.',
  timeout:
    'The server did not respond in time. Check the address and network. If registration was submitted, try logging in before registering again.',
  unreachable:
    'Cannot reach both the voice and HTTPS services. Check the address, ports, and firewall.',
  disconnected: 'No saved session is available. Log in again.',
}
export function friendlyError(caught: unknown): string {
  const error = caught as Partial<ApiError> | null
  return (
    (error?.code && errors[error.code]) || error?.message || 'Connection failed. Please try again.'
  )
}
export const groupedFingerprint = (value: string) =>
  value
    .replace(/:/g, '')
    .match(/.{1,4}/g)
    ?.join(' ') ?? value

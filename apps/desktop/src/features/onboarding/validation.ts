import type { ApiError, ProbeInput } from '../../lib/bindings'

/** `ss_net::DEFAULT_PORT`: QUIC on UDP and HTTPS on TCP, as `ss-server init` writes. */
export const DEFAULT_PORT = 443

export class InvalidHttpsPort extends Error {
  constructor() {
    super('Enter an HTTPS port between 1 and 65535.')
  }
}
export function parseAddress(value: string, httpsPort?: string): ProbeInput {
  const raw = value.trim().replace(/^ss:\/\//i, '')
  if (!raw || /[\s/@?#]/.test(raw) || raw.includes('://'))
    throw new Error('Enter a host, host:port, or ss://host:port.')
  const match = raw.match(/^(\[[a-f\d:]+\]|[^:]+)(?::(\d+))?$/i)
  if (!match || !match[1]) throw new Error('Use [IPv6]:port for an IPv6 address.')
  const port = match[2] ? Number(match[2]) : DEFAULT_PORT
  const https = httpsPort ? Number(httpsPort) : port
  if (!Number.isInteger(port) || port < 1 || port > 65535)
    throw new Error('Ports must be between 1 and 65535.')
  if (httpsPort && (!/^\d+$/.test(httpsPort) || https < 1 || https > 65535))
    throw new InvalidHttpsPort()
  return { host: match[1].replace(/^\[|\]$/g, '').toLowerCase(), port, httpsPort: https }
}
export type AccountField = 'username' | 'password' | 'invite'
export function accountErrors(
  username: string,
  password: string,
  register: boolean,
  inviteRequired: boolean,
  invite: string,
): Partial<Record<AccountField, string>> {
  const errors: Partial<Record<AccountField, string>> = {}
  if (!username.trim()) errors.username = 'Enter your server username.'
  else if (register && !/^[a-z0-9_.]{3,32}$/.test(username.trim()))
    errors.username = 'Use 3–32 lowercase letters, digits, underscores or dots.'
  const length = new TextEncoder().encode(password).length
  if (!password) errors.password = 'Enter your password.'
  else if (length > 128 || (register && length < 8))
    errors.password = 'Use a password between 8 and 128 bytes.'
  if (inviteRequired && !invite.trim()) errors.invite = 'Enter the invite code from your host.'
  return errors
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

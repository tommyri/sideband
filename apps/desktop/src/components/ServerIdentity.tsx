import { useEffect, useState } from 'react'
import { getPublicServerIcon } from '../lib/api'
import type { ProbeInput } from '../lib/bindings'
import { initials } from '../lib/initials'
import './server-identity.css'

type Props = { name: string; target: ProbeInput; hasPublicIcon: boolean }

export function ServerIdentity({ name, target, hasPublicIcon }: Props) {
  const host = target.host.includes(':') ? `[${target.host}]` : target.host
  const address = `${host}:${target.port}`
  return (
    <div className="server-preview">
      <ServerMark
        key={`${address}:${target.httpsPort}:${hasPublicIcon}`}
        name={name}
        target={target}
        hasPublicIcon={hasPublicIcon}
      />
      <span className="server-preview-label">
        <strong>{name}</strong>
        <span>{address}</span>
      </span>
    </div>
  )
}

function ServerMark({ name, target, hasPublicIcon }: Props) {
  const [icon, setIcon] = useState<string | null>(null)
  const { host, port, httpsPort } = target
  useEffect(() => {
    if (!hasPublicIcon) return
    let current = true
    void getPublicServerIcon({ host, port, httpsPort })
      .then((src) => {
        if (current) setIcon(src)
      })
      .catch(() => undefined)
    return () => {
      current = false
    }
  }, [host, port, httpsPort, hasPublicIcon])
  return (
    <span aria-hidden="true" className="server-preview-mark">
      {icon ? <img src={icon} alt="" onError={() => setIcon(null)} /> : initials(name)}
    </span>
  )
}

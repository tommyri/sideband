import { useEffect, useState } from 'react'
import { isTauri } from '../../../lib/tauri'
import { listenForPairing } from './api'

export function usePairingLink() {
  const [link, setLink] = useState('')
  useEffect(() => {
    if (!isTauri()) return
    let active = true
    const stop = listenForPairing((value) => {
      if (active) setLink(value)
    }).catch(() => undefined)
    return () => {
      active = false
      void stop.then((fn) => fn?.())
    }
  }, [])
  return { link, clear: () => setLink('') }
}

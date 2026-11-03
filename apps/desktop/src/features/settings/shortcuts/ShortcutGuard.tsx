import { useEffect } from 'react'
import { invokeBridge } from '../../../lib/tauri'
import { refreshShortcuts } from './store'
let revision = 0
export function ShortcutGuard() {
  useEffect(() => {
    let last: boolean | undefined
    const update = () => {
      const privateInput =
        document.activeElement instanceof HTMLInputElement &&
        document.activeElement.type === 'password'
      const suspended = privateInput || !!document.querySelector('[data-binding-capture="true"]')
      if (last === suspended) return
      last = suspended
      void invokeBridge('suspend_shortcuts', {
        suspended,
        revision: (revision = Math.max(revision + 1, Date.now() * 1000)),
      }).catch(() => {})
    }
    const observer = new MutationObserver(update)
    observer.observe(document.body, {
      subtree: true,
      attributes: true,
      attributeFilter: ['data-binding-capture'],
      childList: true,
    })
    document.addEventListener('focusin', update, true)
    const out = () => queueMicrotask(update)
    document.addEventListener('focusout', out, true)
    update()
    void refreshShortcuts().catch(() => {})
    const timer = setInterval(() => void refreshShortcuts().catch(() => {}), 2000)
    return () => {
      observer.disconnect()
      document.removeEventListener('focusin', update, true)
      document.removeEventListener('focusout', out, true)
      clearInterval(timer)
    }
  }, [])
  return null
}

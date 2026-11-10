import type { SearchQuery } from '../../lib/generated/SearchQuery'
import type { SearchResult } from '../../lib/generated/SearchResult'
import { invokeBridge } from '../../lib/tauri'

export const searchMessages = (serverId: string, query: SearchQuery) =>
  invokeBridge<SearchResult[]>('search_messages', { serverId, query })

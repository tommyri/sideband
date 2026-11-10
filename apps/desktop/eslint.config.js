import js from '@eslint/js'
import reactHooks from 'eslint-plugin-react-hooks'
import reactRefresh from 'eslint-plugin-react-refresh'
import globals from 'globals'
import tseslint from 'typescript-eslint'

// The webview has no network access of its own: Rust owns every connection,
// so certificate pins and bearer tokens never reach script.
const network = 'The webview never opens connections; add a Rust command instead.'

export default tseslint.config(
  { ignores: ['dist', 'src/lib/generated'] },
  {
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    files: ['**/*.{ts,tsx}'],
    languageOptions: {
      ecmaVersion: 2022,
      globals: globals.browser,
    },
    plugins: {
      'react-hooks': reactHooks,
      'react-refresh': reactRefresh,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      'react-refresh/only-export-components': ['warn', { allowConstantExport: true }],
    },
  },
  {
    files: ['src/**/*.{ts,tsx}'],
    rules: {
      'no-restricted-globals': [
        'error',
        ...['fetch', 'XMLHttpRequest', 'WebSocket', 'EventSource', 'WebTransport'].map((name) => ({
          name,
          message: network,
        })),
      ],
      'no-restricted-properties': [
        'error',
        ...['window', 'globalThis', 'self'].flatMap((object) =>
          ['fetch', 'XMLHttpRequest', 'WebSocket', 'EventSource', 'WebTransport'].map(
            (property) => ({ object, property, message: network }),
          ),
        ),
        { object: 'navigator', property: 'sendBeacon', message: network },
      ],
    },
  },
  {
    // Components reach Tauri through their feature's API module or src/lib/api.ts.
    files: ['src/**/*.tsx'],
    ignores: ['src/**/*.test.tsx'],
    rules: {
      'no-restricted-imports': [
        'error',
        {
          patterns: [
            {
              group: ['@tauri-apps/*'],
              message: 'Call Tauri from an API module (a feature api.ts or src/lib/api.ts).',
            },
            {
              group: ['**/lib/tauri'],
              importNames: ['invokeBridge'],
              message: 'Call Tauri from an API module (a feature api.ts or src/lib/api.ts).',
            },
          ],
        },
      ],
    },
  },
)

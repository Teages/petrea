import { describe, expect, it } from 'vitest'
import { transpile } from '../parity'

describe('replace', () => {
  describe('validation', () => {
    it('rejects an empty key', () => {
      expect(() => transpile('console.log(1)', { replace: { '': '1' } })).toThrow(
        'invalid replace: invalid replace key "": must not be empty',
      )
    })

    it('accepts any string value, including the empty one', () => {
      expect(transpile('log(KEY)', { replace: { KEY: '' } })).toBe('log()')
    })
  })
})

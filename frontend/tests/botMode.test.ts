import { describe, expect, test } from 'bun:test'
import { botHasActiveWork } from '../src/pages/BotsPage'

describe('Bot Mode roster state', () => {
  test('does not label a restored idle session as Working', () => {
    expect(botHasActiveWork('bot-1', [
      { bot_id: 'bot-1', live: false, status: 'processing' },
    ])).toBe(false)
  })

  test('labels only a live processing session for the same Bot as Working', () => {
    expect(botHasActiveWork('bot-1', [
      { bot_id: 'bot-2', live: true, status: 'processing' },
      { bot_id: 'bot-1', live: true, status: 'completed' },
      { bot_id: 'bot-1', live: true, status: 'processing' },
    ])).toBe(true)
  })
})

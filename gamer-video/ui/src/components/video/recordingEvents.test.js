import { describe, expect, it } from 'vitest'
import { eventMediaPosition } from './recordingEvents'
describe('recorded media PTS', () => {
  it('does not add raw Android capture PTS to zero-based MP4 PTS', () => {
    const segment = { media_id: 'media', start_us: 2000, duration_us: 100000, base_pts_us: 90000000 }
    expect(eventMediaPosition({ timeline_us: 5000 }, [segment]).pts_us).toBe(3000)
  })
  it('does not fabricate timing across recording gaps', () => {
    expect(eventMediaPosition({ timeline_us: 5000 }, [{ media_id: 'media', start_us: 10000, duration_us: 1000, base_pts_us: 999 }])).toBeNull()
  })
})

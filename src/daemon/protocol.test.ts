import { describe, expect, it } from 'vitest';
import { isChatGuid } from './protocol.js';

describe('isChatGuid', () => {
  it('recognises the guids Messages writes', () => {
    expect(isChatGuid('iMessage;-;+13105551234')).toBe(true);
    expect(isChatGuid('iMessage;+;chat9')).toBe(true);
    expect(isChatGuid('SMS;-;+18885550000')).toBe(true);
    expect(isChatGuid('iMessage;-;someone@example.com')).toBe(true);
  });

  it('does not mistake a chat name that contains a semicolon', () => {
    // `send` skips resolution for a guid, so a name matched here would be
    // handed to AppleScript as an address and reach nobody.
    expect(isChatGuid('Lunch; also dinner')).toBe(false);
    expect(isChatGuid('a;b;c')).toBe(false);
    expect(isChatGuid(';-;x')).toBe(false);
    expect(isChatGuid('iMessage;-;')).toBe(false);
    expect(isChatGuid('iMessage;x;chat9')).toBe(false);
  });

  it('does not mistake an ordinary name or a rowid', () => {
    expect(isChatGuid('Ship Room')).toBe(false);
    expect(isChatGuid('42')).toBe(false);
    expect(isChatGuid('+13105551234')).toBe(false);
  });
});

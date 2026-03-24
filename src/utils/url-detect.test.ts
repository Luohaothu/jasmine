import { describe, it, expect } from 'vitest';
import { extractUrls } from './url-detect';

describe('extractUrls', () => {
  it('should extract simple URLs', () => {
    expect(extractUrls('Check out https://example.com')).toEqual(['https://example.com']);
  });

  it('should handle multiple URLs', () => {
    expect(extractUrls('Here is http://a.com and https://b.com')).toEqual([
      'http://a.com',
      'https://b.com',
    ]);
  });

  it('should ignore trailing punctuation', () => {
    expect(extractUrls('Go to https://example.com.')).toEqual(['https://example.com']);
    expect(extractUrls('Is it https://example.com?')).toEqual(['https://example.com']);
    expect(extractUrls('Look at https://example.com!')).toEqual(['https://example.com']);
    expect(extractUrls('(https://example.com)')).toEqual(['https://example.com']);
    expect(extractUrls('URL: https://example.com, done')).toEqual(['https://example.com']);
  });

  it('should not extract invalid URLs', () => {
    expect(extractUrls('Just a string without links.')).toEqual([]);
    expect(extractUrls('ftp://not-supported.com')).toEqual([]);
  });
});

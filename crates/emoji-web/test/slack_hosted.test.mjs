import assert from 'node:assert/strict';
import test from 'node:test';

globalThis.window = {
  location: {
    origin: 'http://localhost',
    pathname: '/',
    search: '',
  },
  localStorage: {
    getItem() {
      return null;
    },
  },
};

const {
  canonicalizeStandardUnified,
  standardEmojiImageFilename,
  standardEmojiImageFilenames,
} = await import('../static/slack_hosted.js');

test('standard emoji filenames strip variation selectors for Twemoji assets', () => {
  assert.equal(canonicalizeStandardUnified('1F39F-FE0F'), '1f39f');
  assert.equal(
    standardEmojiImageFilename({ image: '1f39f-fe0f.png' }, '1f39f'),
    '1f39f.png',
  );

  assert.equal(canonicalizeStandardUnified('26D3-FE0F'), '26d3');
  assert.equal(
    standardEmojiImageFilename({ image: '26d3-fe0f.png' }, '26d3'),
    '26d3.png',
  );
});

test('standard emoji filenames normalize keycap codepoint padding', () => {
  assert.equal(canonicalizeStandardUnified('0023-FE0F-20E3'), '23-20e3');
  assert.equal(
    standardEmojiImageFilename({ image: '0023-fe0f-20e3.png' }, '23-20e3'),
    '23-20e3.png',
  );
});

test('standard emoji filenames preserve non-variation codepoints', () => {
  assert.equal(canonicalizeStandardUnified('1F3C3-200D-2640-FE0F'), '1f3c3-200d-2640-fe0f');
  assert.equal(
    standardEmojiImageFilename(
      { image: '1f3c3-200d-2640-fe0f.png' },
      '1f3c3-200d-2640-fe0f',
    ),
    '1f3c3-200d-2640-fe0f.png',
  );
});

test('standard emoji filenames include a stripped fallback for mixed ZWJ data', () => {
  assert.deepEqual(
    standardEmojiImageFilenames(
      { image: '1f441-fe0f-200d-1f5e8-fe0f.png' },
      '1f441-fe0f-200d-1f5e8-fe0f',
    ),
    [
      '1f441-fe0f-200d-1f5e8-fe0f.png',
      '1f441-200d-1f5e8.png',
    ],
  );
});

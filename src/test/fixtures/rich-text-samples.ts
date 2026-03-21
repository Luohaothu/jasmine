export interface MentionSample {
  user_id: string;
  display_name: string;
  start?: number;
  end?: number;
}

export interface UrlSample {
  url: string;
  start?: number;
  end?: number;
}

export interface RichTextSample {
  input: string;
  expected_mentions: MentionSample[];
  expected_urls: UrlSample[];
}

export const richTextSamples: RichTextSample[] = [
  {
    input: "Hello @[Alice](user:uuid-1)",
    expected_mentions: [{ user_id: "uuid-1", display_name: "Alice", start: 6, end: 27 }],
    expected_urls: [],
  },
  {
    input: "@[Alice](user:uuid-1) and @[Bob](user:uuid-2)",
    expected_mentions: [
      { user_id: "uuid-1", display_name: "Alice", start: 0, end: 21 },
      { user_id: "uuid-2", display_name: "Bob", start: 33, end: 52 },
    ],
    expected_urls: [],
  },
  {
    input: "**@[Alice](user:uuid-1)**",
    expected_mentions: [{ user_id: "uuid-1", display_name: "Alice", start: 2, end: 23 }],
    expected_urls: [],
  },
  {
    input: "Hello @[Nope(user:bad-1) world",
    expected_mentions: [],
    expected_urls: [],
  },
  {
    input: "Hi @[](user:uuid-1)",
    expected_mentions: [],
    expected_urls: [],
  },
  {
    input: "Hi @[NoUser](user:)",
    expected_mentions: [],
    expected_urls: [],
  },
  {
    input: "Visit https://example.com for details",
    expected_mentions: [],
    expected_urls: [{ url: "https://example.com", start: 6, end: 25 }],
  },
  {
    input: "See [label](https://example.org/docs?x=1)",
    expected_mentions: [],
    expected_urls: [{ url: "https://example.org/docs?x=1", start: 12, end: 40 }],
  },
  {
    input: "Two links: http://a.com and https://b.io/docs.",
    expected_mentions: [],
    expected_urls: [
      { url: "http://a.com", start: 11, end: 23 },
      { url: "https://b.io/docs", start: 28, end: 45 },
    ],
  },
  {
    input: "Trailing punctuation: https://x.io?y=1, https://y.io!)",
    expected_mentions: [],
    expected_urls: [
      { url: "https://x.io?y=1", start: 22, end: 38 },
      { url: "https://y.io", start: 40, end: 52 },
    ],
  },
  {
    input: "Edge @[](user:bad-9) and @[Carol](user:carol-1)",
    expected_mentions: [{ user_id: "carol-1", display_name: "Carol", start: 25, end: 47 }],
    expected_urls: [],
  },
];

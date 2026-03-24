export function extractUrls(text: string): string[] {
  if (!text) return [];
  const urlRegex = /https?:\/\/[^\s]+/g;
  const matches = text.match(urlRegex) || [];

  return matches.map((match) => {
    return match.replace(/[.,!?;:'"()]+$/, '');
  });
}

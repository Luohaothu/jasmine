import { render, screen } from '@testing-library/react';
import { describe, it, expect } from 'vitest';
import { RichTextRenderer } from '../RichTextRenderer';
import { richTextSamples } from '../../../test/fixtures/rich-text-samples';
import '../../../i18n/i18n';

describe('RichTextRenderer', () => {
  it('renders basic text and standard markdown correctly', () => {
    render(<RichTextRenderer content="Hello **bold** and *italic* with `code`" />);

    expect(screen.getByText('Hello', { exact: false })).toBeInTheDocument();

    const boldEl = screen.getByText('bold');
    expect(boldEl.tagName).toBe('STRONG');

    const italicEl = screen.getByText('italic');
    expect(italicEl.tagName).toBe('EM');

    const codeEl = screen.getByText('code');
    expect(codeEl.tagName).toBe('CODE');
  });

  it('safely sanitizes XSS scripts', () => {
    render(<RichTextRenderer content={'[malicious](javascript:alert("xss"))'} />);

    const link = screen.getByText('malicious');
    expect(link).not.toHaveAttribute('href');
  });

  it('preserves safe trailing text after stripped malicious tags', () => {
    const { container } = render(
      <RichTextRenderer content={'<script>alert(1)</script>Hello Safe Text'} />
    );

    // The script should not be rendered as HTML, but the trailing text should remain
    expect(screen.getByText('Hello Safe Text', { exact: false })).toBeInTheDocument();
    expect(container.innerHTML).not.toContain('<script>');
  });

  it('enforces strict markdown subset by stripping tables', () => {
    const { container } = render(
      <RichTextRenderer
        content={`| Col 1 | Col 2 |
|---|---|
| A | B |`}
      />
    );
    expect(container.querySelector('table')).not.toBeInTheDocument();
  });

  it('renders urls as link cards', () => {
    const { container } = render(
      <RichTextRenderer content={'Check out [my site](https://example.com)'} />
    );
    const link = container.querySelector("a[data-testid='link-card']");
    expect(link).toBeInTheDocument();
    expect(link).toHaveAttribute('href', 'https://example.com');
    expect(link).toHaveAttribute('aria-label', 'Open link: https://example.com');
    expect(link?.textContent).toContain('my site');
    expect(link?.textContent).toContain('https://example.com');
  });

  describe('renders fixtures correctly', () => {
    richTextSamples.forEach((sample, index) => {
      it(`handles fixture ${index}: ${sample.input.substring(0, 30)}...`, () => {
        const { container } = render(<RichTextRenderer content={sample.input} />);

        sample.expected_mentions.forEach((mention) => {
          const mentionEl = screen.getByText(`@${mention.display_name}`);
          expect(mentionEl).toBeInTheDocument();
          expect(mentionEl.tagName).toBe('SPAN');
          expect(mentionEl.className).toMatch(/mention/);
          expect(mentionEl).toHaveAttribute('data-mention', `user:${mention.user_id}`);
        });

        const allMentions = container.querySelectorAll('span[data-mention]');
        expect(allMentions.length).toBe(sample.expected_mentions.length);

        sample.expected_urls.forEach((urlInfo) => {
          const linkEl = container.querySelector(`a[href="${urlInfo.url}"]`);
          expect(linkEl).toBeInTheDocument();
        });

        const allLinks = Array.from(container.querySelectorAll('a')).filter(
          (a) => !a.href.startsWith('user:') && a.href !== ''
        );
        expect(allLinks.length).toBe(sample.expected_urls.length);
      });
    });
  });
});

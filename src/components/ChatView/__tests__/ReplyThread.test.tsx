import { render, screen, fireEvent } from '@testing-library/react';
import { describe, it, expect, vi } from 'vitest';
import { ReplyThread } from '../ReplyThread';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, params?: any) => {
      if (key === 'chat.messageBubble.jumpToReply') {
        return `Jump to: ${params?.preview}`;
      }
      return key;
    },
  }),
}));

describe('ReplyThread', () => {
  it('renders the thread correctly', () => {
    render(
      <ReplyThread replyToId="msg-123" replyToPreview="Hello world">
        <div data-testid="child-message">Reply 1</div>
        <div data-testid="child-message">Reply 2</div>
      </ReplyThread>
    );

    expect(screen.getByTestId('reply-thread')).toBeInTheDocument();
    expect(screen.getByText('Hello world')).toBeInTheDocument();
    expect(screen.getAllByTestId('child-message')).toHaveLength(2);
  });

  it('scrolls to parent message when quote is clicked', () => {
    const scrollIntoViewMock = vi.fn();
    const mockElement = document.createElement('div');
    mockElement.id = 'message-msg-123';
    mockElement.scrollIntoView = scrollIntoViewMock;
    document.body.appendChild(mockElement);

    render(
      <ReplyThread replyToId="msg-123" replyToPreview="Hello world">
        <div>Reply 1</div>
      </ReplyThread>
    );

    const quoteButton = screen.getByTestId('thread-quote');
    fireEvent.click(quoteButton);

    expect(scrollIntoViewMock).toHaveBeenCalledWith({ behavior: 'smooth', block: 'center' });

    document.body.removeChild(mockElement);
  });
});

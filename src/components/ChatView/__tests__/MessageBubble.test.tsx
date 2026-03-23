import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { MessageBubble } from '../MessageBubble';
import styles from '../MessageBubble.module.css';

describe('MessageBubble', () => {
  const testDate = new Date();
  testDate.setHours(14, 5, 0, 0);
  const timestamp = testDate.getTime();

  it('renders own message with sent status, proper alignment, blue styling, and exact HH:mm', () => {
    const { container } = render(
      <MessageBubble
        id="msg-1"
        content="Hello there"
        timestamp={timestamp}
        isOwn={true}
        status="sent"
      />
    );
    expect(screen.getByText('Hello there')).toBeInTheDocument();

    expect(screen.getByText('14:05')).toBeInTheDocument();

    expect(screen.getByText('✓')).toBeInTheDocument();

    const wrapper = container.firstElementChild;
    expect(wrapper).toHaveClass(styles.own);

    const bubble = wrapper?.querySelector(`.${styles.bubble}`);
    expect(bubble).toHaveClass(styles.ownBubble);
  });

  it('renders own message with delivered status', () => {
    render(
      <MessageBubble
        id="msg-1"
        content="Hello there"
        timestamp={timestamp}
        isOwn={true}
        status="delivered"
      />
    );
    expect(screen.getByText('✓✓')).toBeInTheDocument();
  });

  it('renders encrypted lock badge only when encrypted is true', () => {
    const { rerender } = render(
      <MessageBubble
        id="msg-1"
        content="Secret hello"
        timestamp={timestamp}
        isOwn={true}
        encrypted={true}
      />
    );

    expect(screen.getByTestId('encrypted-badge')).toBeInTheDocument();

    rerender(
      <MessageBubble
        id="msg-1"
        content="Legacy hello"
        timestamp={timestamp}
        isOwn={true}
        encrypted={false}
      />
    );
    expect(screen.queryByTestId('encrypted-badge')).not.toBeInTheDocument();

    rerender(<MessageBubble id="msg-1" content="No flag" timestamp={timestamp} isOwn={true} />);
    expect(screen.queryByTestId('encrypted-badge')).not.toBeInTheDocument();
  });

  it('renders own message with failed status', () => {
    render(
      <MessageBubble
        id="msg-1"
        content="Hello there"
        timestamp={timestamp}
        isOwn={true}
        status="failed"
      />
    );
    expect(screen.getByText('❌')).toBeInTheDocument();
  });

  it('renders peer message with proper alignment and gray styling without status icons', () => {
    const { container } = render(
      <MessageBubble id="msg-1" content="Peer message" timestamp={timestamp} isOwn={false} />
    );
    expect(screen.getByText('Peer message')).toBeInTheDocument();
    expect(screen.queryByText('✓')).not.toBeInTheDocument();
    expect(screen.queryByText('✓✓')).not.toBeInTheDocument();
    expect(screen.queryByText('❌')).not.toBeInTheDocument();

    const wrapper = container.firstElementChild;
    expect(wrapper).toHaveClass(styles.peer);

    const bubble = wrapper?.querySelector(`.${styles.bubble}`);
    expect(bubble).toHaveClass(styles.peerBubble);
  });

  it('renders tombstone for deleted message', () => {
    const { container } = render(
      <MessageBubble
        id="msg-1"
        content="Hello there"
        timestamp={timestamp}
        isOwn={true}
        isDeleted={true}
      />
    );
    expect(screen.getByText('This message was deleted')).toBeInTheDocument();
    expect(screen.queryByText('Hello there')).not.toBeInTheDocument();

    const wrapper = container.firstElementChild;
    const bubble = wrapper?.querySelector(`.${styles.bubble}`);
    expect(bubble).toHaveClass(styles.tombstone);
  });

  it('shows edited mark if editedAt is provided', () => {
    render(
      <MessageBubble
        id="msg-1"
        content="Hello there"
        timestamp={timestamp}
        isOwn={true}
        editedAt={12345678}
      />
    );
    expect(screen.getByText('(edited)')).toBeInTheDocument();
  });

  it('can enter edit mode and save', () => {
    const onEdit = vi.fn();
    render(
      <MessageBubble
        id="msg-1"
        content="Hello there"
        timestamp={timestamp}
        isOwn={true}
        onEdit={onEdit}
      />
    );

    const editButton = screen.getByText('Edit');
    fireEvent.click(editButton);

    const textarea = screen.getByRole('textbox');
    fireEvent.change(textarea, { target: { value: 'Hello edited' } });

    const saveButton = screen.getByText('Save');
    fireEvent.click(saveButton);

    expect(onEdit).toHaveBeenCalledWith('msg-1', 'Hello edited');
  });

  it('can enter edit mode and cancel', () => {
    const onEdit = vi.fn();
    render(
      <MessageBubble
        id="msg-1"
        content="Hello there"
        timestamp={timestamp}
        isOwn={true}
        onEdit={onEdit}
      />
    );

    const editButton = screen.getByText('Edit');
    fireEvent.click(editButton);

    const textarea = screen.getByRole('textbox');
    fireEvent.change(textarea, { target: { value: 'Hello edited' } });

    const cancelButton = screen.getByText('Cancel');
    fireEvent.click(cancelButton);

    expect(onEdit).not.toHaveBeenCalled();
    expect(screen.getByText('Hello there')).toBeInTheDocument();
  });

  it('triggers onDelete when delete button is clicked and confirmed', () => {
    const onDelete = vi.fn();
    window.confirm = vi.fn(() => true);

    render(
      <MessageBubble
        id="msg-1"
        content="Hello there"
        timestamp={timestamp}
        isOwn={true}
        onDelete={onDelete}
      />
    );

    const deleteButton = screen.getByText('Delete');
    fireEvent.click(deleteButton);

    expect(window.confirm).toHaveBeenCalledWith('Are you sure you want to delete this message?');
    expect(onDelete).toHaveBeenCalledWith('msg-1');
  });

  it('does not trigger onDelete if delete is not confirmed', () => {
    const onDelete = vi.fn();
    window.confirm = vi.fn(() => false);

    render(
      <MessageBubble
        id="msg-1"
        content="Hello there"
        timestamp={timestamp}
        isOwn={true}
        onDelete={onDelete}
      />
    );

    const deleteButton = screen.getByText('Delete');
    fireEvent.click(deleteButton);

    expect(window.confirm).toHaveBeenCalled();
    expect(onDelete).not.toHaveBeenCalled();
  });

  it('does not show edit/delete buttons for peer messages', () => {
    render(<MessageBubble id="msg-1" content="Peer message" timestamp={timestamp} isOwn={false} />);
    expect(screen.queryByText('Edit')).not.toBeInTheDocument();
    expect(screen.queryByText('Delete')).not.toBeInTheDocument();
  });

  describe('Quote Reply', () => {
    it('renders quote bubble if replyToId and replyToPreview are present', () => {
      render(
        <MessageBubble
          id="msg-1"
          content="Hello World"
          timestamp={timestamp}
          isOwn={true}
          replyToId="msg-parent"
          replyToPreview="Parent preview content"
        />
      );
      expect(screen.getByTestId('quote-bubble')).toBeInTheDocument();
      expect(screen.getByText('Parent preview content')).toBeInTheDocument();
    });

    it('triggers onReply when reply action button is clicked', () => {
      const mockOnReply = vi.fn();
      render(
        <MessageBubble
          id="msg-1"
          content="Hello World"
          timestamp={timestamp}
          isOwn={false}
          onReply={mockOnReply}
        />
      );

      const replyButton = screen.getByTestId('action-reply');
      fireEvent.click(replyButton);

      expect(mockOnReply).toHaveBeenCalledWith('msg-1', 'Hello World', undefined);
    });

    it('truncates reply preview string if content is too long', () => {
      const mockOnReply = vi.fn();
      const longContent = 'A'.repeat(150);
      render(
        <MessageBubble
          id="msg-1"
          content={longContent}
          timestamp={timestamp}
          isOwn={true}
          onReply={mockOnReply}
        />
      );

      const replyButton = screen.getByTestId('action-reply');
      fireEvent.click(replyButton);

      expect(mockOnReply).toHaveBeenCalledWith('msg-1', 'A'.repeat(80) + '…', undefined);
    });
  });
});

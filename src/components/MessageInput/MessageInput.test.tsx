import { fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { MessageInput } from './MessageInput';
import { usePeerStore } from '../../stores/peerStore';
import { useChatStore } from '../../stores/chatStore';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('../../stores/peerStore', () => ({
  usePeerStore: vi.fn(),
}));

describe('MessageInput', () => {
  const peerId = 'peer-123';

  beforeEach(() => {
    vi.clearAllMocks();
    (usePeerStore as any).mockReturnValue({
      peers: [
        { id: 'uuid-1', name: 'Alice', status: 'online' },
        { id: 'uuid-2', name: 'Bob', status: 'offline' },
      ],
    });
  });

  it('renders textarea and send button', () => {
    render(<MessageInput peerId={peerId} />);
    expect(screen.getByPlaceholderText(/type a message/i)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /send/i })).toBeInTheDocument();
  });

  describe('MessageInput.empty', () => {
    it('disables send button when input is empty or whitespace', () => {
      render(<MessageInput peerId={peerId} />);
      const button = screen.getByRole('button', { name: /send/i });
      expect(button).toBeDisabled();

      const input = screen.getByPlaceholderText(/type a message/i);
      fireEvent.change(input, { target: { value: '   \n ' } });
      expect(button).toBeDisabled();
    });
  });

  describe('MessageInput.send', () => {
    it('enables send button and sends message on click', async () => {
      const user = userEvent.setup();
      render(<MessageInput peerId={peerId} />);

      const input = screen.getByPlaceholderText(/type a message/i);
      const button = screen.getByRole('button', { name: /send/i });

      await user.type(input, 'Hello world!');
      expect(button).not.toBeDisabled();

      await user.click(button);

      expect(invoke).toHaveBeenCalledWith('send_message', {
        peerId,
        content: 'Hello world!',
        replyToId: null,
      });
      expect(input).toHaveValue('');
    });

    it('sends message on Enter and Ctrl+Enter', async () => {
      const user = userEvent.setup();
      render(<MessageInput peerId={peerId} />);
      const input = screen.getByPlaceholderText(/type a message/i);

      await user.type(input, 'Test Enter{Enter}');
      expect(invoke).toHaveBeenCalledWith('send_message', {
        peerId,
        content: 'Test Enter',
        replyToId: null,
      });
      expect(input).toHaveValue('');

      await user.type(input, 'Test Ctrl Enter{Control>}{Enter}{/Control}');
      expect(invoke).toHaveBeenCalledWith('send_message', {
        peerId,
        content: 'Test Ctrl Enter',
        replyToId: null,
      });
      expect(input).toHaveValue('');
    });

    it('does not send on Shift+Enter (inserts newline)', async () => {
      const user = userEvent.setup();
      render(<MessageInput peerId={peerId} />);
      const input = screen.getByPlaceholderText(/type a message/i);

      await user.type(input, 'Line 1{Shift>}{Enter}{/Shift}Line 2');
      expect(invoke).not.toHaveBeenCalled();
      expect(input).toHaveValue('Line 1\nLine 2');
    });
  });

  describe('MessageInput.drag', () => {
    it('shows drop overlay when file is dragged over', () => {
      render(<MessageInput peerId={peerId} />);

      const container = screen.getByTestId('message-input-container');

      fireEvent.dragOver(container, { dataTransfer: { types: ['Files'] } });

      expect(screen.getByText(/释放以发送文件/i)).toBeInTheDocument();

      fireEvent.dragLeave(container);
      expect(screen.queryByText(/释放以发送文件/i)).not.toBeInTheDocument();
    });

    it('invokes file transfer on file drop', async () => {
      render(<MessageInput peerId={peerId} />);

      const container = screen.getByTestId('message-input-container');

      const file1 = new File(['test1'], 'file1.txt', { type: 'text/plain' });
      const file2 = new File(['test2'], 'file2.jpg', { type: 'image/jpeg' });

      fireEvent.drop(container, {
        dataTransfer: {
          types: ['Files'],
          files: [file1, file2],
        },
      });

      await vi.waitFor(() => {
        expect(invoke).toHaveBeenCalledWith('send_file', {
          peerId,
          filePath: 'file1.txt',
        });
        expect(invoke).toHaveBeenCalledWith('send_file', {
          peerId,
          filePath: 'file2.jpg',
        });
      });
    });
  });

  describe('Quote Reply', () => {
    beforeEach(() => {
      useChatStore.setState({
        replyingTo: { id: 'msg-123', preview: 'This is a preview text' }
      });
    });

    afterEach(() => {
      useChatStore.setState({ replyingTo: null });
    });

    it('renders reply preview bar when replyingTo is set', () => {
      render(<MessageInput peerId={peerId} />);
      expect(screen.getByTestId('reply-preview-bar')).toBeInTheDocument();
      expect(screen.getByText('This is a preview text')).toBeInTheDocument();
    });

    it('cancels reply when cancel button is clicked', async () => {
      const user = userEvent.setup();
      render(<MessageInput peerId={peerId} />);
      const cancelButton = screen.getByTestId('cancel-reply-button');
      await user.click(cancelButton);
      expect(useChatStore.getState().replyingTo).toBeNull();
    });

    it('sends message with replyToId and clears replyingTo', async () => {
      const user = userEvent.setup();
      render(<MessageInput peerId={peerId} />);
      const input = screen.getByPlaceholderText(/type a message/i);
      const button = screen.getByRole('button', { name: /send/i });

      await user.type(input, 'Replying to you');
      await user.click(button);

      expect(invoke).toHaveBeenCalledWith('send_message', {
        peerId,
        content: 'Replying to you',
        replyToId: 'msg-123',
      });
      expect(useChatStore.getState().replyingTo).toBeNull();
    });
  });

  describe('Character limit', () => {
    it('prevents typing beyond 10000 characters', () => {
      render(<MessageInput peerId={peerId} />);
      const input = screen.getByPlaceholderText(/type a message/i) as HTMLTextAreaElement;

      expect(input.maxLength).toBe(10000);
    });

    it('shows remaining characters when near limit', () => {
      render(<MessageInput peerId={peerId} />);
      const input = screen.getByPlaceholderText(/type a message/i);

      const longText = 'a'.repeat(9950);
      fireEvent.change(input, { target: { value: longText } });

      expect(screen.getByText(/50/)).toBeInTheDocument();
    });
  });

  describe('Typing indicator', () => {
    beforeEach(() => {
      vi.useFakeTimers();
    });

    afterEach(() => {
      vi.useRealTimers();
    });

    it('sends typing indicator with debounce', () => {
      render(<MessageInput peerId={peerId} />);

      const input = screen.getByPlaceholderText(/type a message/i);
      fireEvent.change(input, { target: { value: 'H' } });

      expect(invoke).not.toHaveBeenCalledWith('send_typing_indicator', expect.any(Object));

      vi.advanceTimersByTime(500);

      expect(invoke).toHaveBeenCalledWith('send_typing_indicator', { peerId });
    });
  });

  describe('Mentions', () => {
    beforeEach(() => {
      (usePeerStore as any).mockImplementation((selector: any) => 
        selector({
          peers: [
            { id: 'uuid-1', name: 'Alice', status: 'online' },
            { id: 'uuid-2', name: 'Bob', status: 'offline' },
          ],
        })
      );
    });

    it('opens autocomplete when @ is typed', async () => {
      const user = userEvent.setup();
      render(<MessageInput peerId={peerId} />);
      const input = screen.getByPlaceholderText(/type a message/i);

      await user.type(input, 'Hello @');
      expect(screen.getByTestId('mention-autocomplete')).toBeInTheDocument();
      expect(screen.getByText('Alice')).toBeInTheDocument();
      expect(screen.getByText('Bob')).toBeInTheDocument();
    });

    it('filters peers when typing after @', async () => {
      const user = userEvent.setup();
      render(<MessageInput peerId={peerId} />);
      const input = screen.getByPlaceholderText(/type a message/i);

      await user.type(input, 'Hello @Al');
      expect(screen.getByTestId('mention-autocomplete')).toBeInTheDocument();
      expect(screen.getByText('Alice')).toBeInTheDocument();
      expect(screen.queryByText('Bob')).not.toBeInTheDocument();
    });

    it('closes on Escape and remembers dismissal', async () => {
      const user = userEvent.setup();
      render(<MessageInput peerId={peerId} />);
      const input = screen.getByPlaceholderText(/type a message/i);

      await user.type(input, '@');
      expect(screen.getByTestId('mention-autocomplete')).toBeInTheDocument();

      await user.type(input, '{Escape}');
      expect(screen.queryByTestId('mention-autocomplete')).not.toBeInTheDocument();

      await user.type(input, 'A');
      expect(screen.queryByTestId('mention-autocomplete')).not.toBeInTheDocument();
    });

    it('inserts mention on Enter and closes autocomplete', async () => {
      const user = userEvent.setup();
      render(<MessageInput peerId={peerId} />);
      const input = screen.getByPlaceholderText(/type a message/i);

      await user.type(input, '@Al');
      expect(screen.getByTestId('mention-autocomplete')).toBeInTheDocument();

      await user.type(input, '{Enter}');
      expect(screen.queryByTestId('mention-autocomplete')).not.toBeInTheDocument();
      expect(input).toHaveValue('@[Alice](user:uuid-1) ');
    });

    it('navigates with arrow keys', async () => {
      const user = userEvent.setup();
      render(<MessageInput peerId={peerId} />);
      const input = screen.getByPlaceholderText(/type a message/i);

      await user.type(input, '@');
      await user.type(input, '{ArrowDown}{Enter}');
      
      expect(input).toHaveValue('@[Bob](user:uuid-2) ');
    });
  });
});

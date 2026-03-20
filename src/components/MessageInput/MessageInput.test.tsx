import { fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { MessageInput } from './MessageInput';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn().mockResolvedValue(undefined),
}));

describe('MessageInput', () => {
  const peerId = 'peer-123';

  beforeEach(() => {
    vi.clearAllMocks();
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
      });
      expect(input).toHaveValue('');

      await user.type(input, 'Test Ctrl Enter{Control>}{Enter}{/Control}');
      expect(invoke).toHaveBeenCalledWith('send_message', {
        peerId,
        content: 'Test Ctrl Enter',
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
});

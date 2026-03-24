import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi, beforeEach, Mock } from 'vitest';
import { mockIPC } from '@tauri-apps/api/mocks';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import Settings from './Settings';
import '../../i18n/i18n';

vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@tauri-apps/api/core')>();
  return {
    ...actual,
    invoke: vi.fn((cmd, args) => actual.invoke(cmd, args)),
  };
});

describe('Settings Component', () => {
  const mockIdentity = {
    device_id: 'test-device-uuid-1234',
    display_name: 'Test User',
    avatar_path: '/path/to/avatar.jpg',
  };

  const mockSettings = {
    download_dir: '/home/test/Downloads/Jasmine',
    max_concurrent_transfers: 3,
  };

  const mockOwnFingerprint = {
    deviceId: 'test-device-uuid-1234',
    fingerprint: 'A1B2 C3D4 E5F6 7788 99AA BBCC DDEE FF00 1122 3344 5566 7788 99AA BBCC DDEE FF00',
  };

  beforeEach(() => {
    vi.clearAllMocks();

    mockIPC((command) => {
      if (command === 'get_identity') return mockIdentity;
      if (command === 'get_settings') return mockSettings;
      if (command === 'update_display_name') return null;
      if (command === 'update_avatar') return null;
      if (command === 'update_settings') return null;
      if (command === 'get_own_fingerprint') return mockOwnFingerprint;
      return null;
    });
  });

  it('loads and displays identity and settings', async () => {
    render(<Settings />);

    await waitFor(() => {
      expect(screen.getByDisplayValue('Test User')).toBeInTheDocument();
    });

    const deviceIdInput = screen.getByLabelText('Device ID', { selector: 'input' });
    expect(deviceIdInput).toHaveValue('test-device-uuid-1234');
    expect(deviceIdInput).toHaveAttribute('readonly');

    const copyButton = screen.getByRole('button', { name: /copy device id/i });
    expect(copyButton).toBeInTheDocument();

    expect(screen.getByText('/home/test/Downloads/Jasmine')).toBeInTheDocument();

    const transferInput = screen.getByLabelText(/max concurrent transfers/i);
    expect(transferInput).toHaveValue(3);

    expect(screen.getByTestId('security-section')).toBeInTheDocument();
    expect(screen.getByTestId('own-fingerprint')).toHaveTextContent(mockOwnFingerprint.fingerprint);
  });

  it('copies device id when copy button is clicked', async () => {
    const user = userEvent.setup();
    render(<Settings />);

    await waitFor(() => {
      expect(screen.getByDisplayValue('Test User')).toBeInTheDocument();
    });

    const copyButton = screen.getByRole('button', { name: /copy device id/i });
    await user.click(copyButton);

    const text = await navigator.clipboard.readText();
    expect(text).toBe('test-device-uuid-1234');
  });

  it('copies local fingerprint when copy fingerprint is clicked', async () => {
    const user = userEvent.setup();
    render(<Settings />);

    await waitFor(() => {
      expect(screen.getByTestId('own-fingerprint')).toHaveTextContent(
        mockOwnFingerprint.fingerprint
      );
    });

    await user.click(screen.getByTestId('copy-fingerprint'));

    const text = await navigator.clipboard.readText();
    expect(text).toBe(mockOwnFingerprint.fingerprint);
  });

  it('updates display name using invoke', async () => {
    const user = userEvent.setup();
    render(<Settings />);

    await waitFor(() => {
      expect(screen.getByDisplayValue('Test User')).toBeInTheDocument();
    });

    const nameInput = screen.getByLabelText(/display name/i);
    await user.clear(nameInput);
    await user.type(nameInput, 'New Name');
    await user.tab();

    expect(invoke).toHaveBeenCalledWith('update_display_name', { name: 'New Name' });
  });

  it('updates avatar using invoke and dialog', async () => {
    const user = userEvent.setup();
    (open as Mock).mockResolvedValueOnce('/new/avatar.png');

    render(<Settings />);

    await waitFor(() => {
      expect(screen.getByText(/change avatar/i)).toBeInTheDocument();
    });

    await user.click(screen.getByText(/change avatar/i));

    expect(open).toHaveBeenCalledWith(
      expect.objectContaining({
        filters: [{ name: 'Images', extensions: ['jpg', 'png', 'gif'] }],
      })
    );

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_avatar', { path: '/new/avatar.png' });
    });
  });

  it('updates download directory using invoke and dialog', async () => {
    const user = userEvent.setup();
    (open as Mock).mockResolvedValueOnce('/new/downloads');

    render(<Settings />);

    await waitFor(() => {
      expect(screen.getByText(/change folder/i)).toBeInTheDocument();
    });

    await user.click(screen.getByText(/change folder/i));

    expect(open).toHaveBeenCalledWith({ directory: true });

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        'update_settings',
        expect.objectContaining({
          settings: expect.objectContaining({
            download_dir: '/new/downloads',
          }),
        })
      );
    });
  });

  it('clamps max concurrent transfers to 1-5', async () => {
    const user = userEvent.setup();
    render(<Settings />);

    await waitFor(() => {
      expect(screen.getByDisplayValue('3')).toBeInTheDocument();
    });

    const input = screen.getByLabelText(/max concurrent transfers/i);

    await user.clear(input);
    await user.type(input, '10');
    await user.tab();

    expect(invoke).toHaveBeenCalledWith(
      'update_settings',
      expect.objectContaining({
        settings: expect.objectContaining({
          max_concurrent_transfers: 5,
        }),
      })
    );

    await user.clear(input);
    await user.type(input, '0');
    await user.tab();

    expect(invoke).toHaveBeenCalledWith(
      'update_settings',
      expect.objectContaining({
        settings: expect.objectContaining({
          max_concurrent_transfers: 1,
        }),
      })
    );
  });
});

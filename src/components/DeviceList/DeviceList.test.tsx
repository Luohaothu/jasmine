import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { BrowserRouter } from 'react-router-dom';
import { mockIPC } from '@tauri-apps/api/mocks';
import { invoke } from '@tauri-apps/api/core';
import { usePeerStore } from '../../stores/peerStore';
import { DeviceList } from './DeviceList';
import '../../i18n/i18n';

vi.mock('@tauri-apps/api/core', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@tauri-apps/api/core')>();
  return {
    ...actual,
    invoke: vi.fn((cmd, args) => actual.invoke(cmd, args)),
  };
});

const mockPeers = [
  { id: 'peer-1', name: 'Alice', status: 'online' as const, warning: undefined },
  { id: 'peer-2', name: 'Bob', status: 'offline' as const, warning: undefined },
  { id: 'peer-3', name: 'Charlie', status: 'online' as const, warning: undefined },
];

const renderDeviceList = () => {
  return render(
    <BrowserRouter>
      <DeviceList />
    </BrowserRouter>
  );
};

describe('DeviceList', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    usePeerStore.setState({ peers: [] });
    mockIPC((command, args) => {
      if (command === 'get_peer_fingerprint') {
        const peerId = (args as { peerId?: string } | undefined)?.peerId;
        if (peerId === 'peer-1') {
          return {
            peerId,
            fingerprint:
              'A1B2 C3D4 E5F6 7788 99AA BBCC DDEE FF00 1122 3344 5566 7788 99AA BBCC DDEE FF00',
            verified: false,
            firstSeen: 1234,
          };
        }
        if (peerId === 'peer-2') {
          return {
            peerId,
            fingerprint:
              'FFFF EEEE DDDD CCCC BBBB AAAA 9999 8888 7777 6666 5555 4444 3333 2222 1111 0000',
            verified: true,
            firstSeen: 5678,
          };
        }
      }
      if (command === 'toggle_peer_verified') {
        return null;
      }
      return null;
    });
  });

  it('renders empty state with spinner when no peers', () => {
    renderDeviceList();
    expect(screen.getByText(/Searching local network devices/i)).toBeInTheDocument();
  });

  it('renders list of peers with avatars and online/offline status indicators', () => {
    usePeerStore.setState({ peers: mockPeers });
    renderDeviceList();

    expect(screen.getByText('Alice')).toBeInTheDocument();
    expect(screen.getByText('Bob')).toBeInTheDocument();
    expect(screen.getByText('Charlie')).toBeInTheDocument();

    expect(screen.getByText('A')).toBeInTheDocument();
    expect(screen.getByText('B')).toBeInTheDocument();
    expect(screen.getByText('C')).toBeInTheDocument();

    const onlineStatuses = screen.getAllByText('online');
    expect(onlineStatuses).toHaveLength(2);

    const offlineStatuses = screen.getAllByText('offline');
    expect(offlineStatuses).toHaveLength(1);
  });

  it('filters peers by search', () => {
    usePeerStore.setState({ peers: mockPeers });
    renderDeviceList();

    const searchInput = screen.getByPlaceholderText(/Search devices/i);
    fireEvent.change(searchInput, { target: { value: 'ali' } });

    expect(screen.getByText('Alice')).toBeInTheDocument();
    expect(screen.queryByText('Bob')).not.toBeInTheDocument();
    expect(screen.queryByText('Charlie')).not.toBeInTheDocument();
  });

  it('navigates to chat when a peer is clicked', () => {
    usePeerStore.setState({ peers: mockPeers });
    renderDeviceList();

    const aliceLink = screen.getByRole('link', { name: /Alice/i });
    expect(aliceLink).toHaveAttribute('href', '/chat/peer-1');
  });

  it('renders peer fingerprint details and toggles verification', async () => {
    usePeerStore.setState({ peers: mockPeers });
    renderDeviceList();

    await waitFor(() => {
      expect(screen.getAllByTestId('peer-fingerprint')).toHaveLength(2);
    });

    expect(screen.getByText(/A1B2 C3D4 E5F6 7788/i)).toBeInTheDocument();
    expect(screen.getByText('Unverified')).toBeInTheDocument();
    expect(screen.getByText('Verified')).toBeInTheDocument();

    fireEvent.click(screen.getAllByTestId('verify-toggle')[0]);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('toggle_peer_verified', {
        peerId: 'peer-1',
        verified: true,
      });
    });
  });

  it('renders incompatible peers with actionable warning text', () => {
    usePeerStore.setState({
      peers: [
        {
          id: 'peer-legacy',
          name: 'Legacy Device',
          status: 'incompatible',
          warning: 'Update required',
        },
      ],
    });

    renderDeviceList();

    expect(screen.getByText('Legacy Device')).toBeInTheDocument();
    expect(screen.getByText('incompatible')).toBeInTheDocument();
    expect(screen.getByText('Update required')).toBeInTheDocument();
  });
});

import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { describe, it, expect, vi } from 'vitest';
import { CreateGroupModal } from './CreateGroupModal';
import { invoke } from '@tauri-apps/api/core';

// Mock Tauri invoke
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

const mockPeers = [
  { id: 'p1', name: 'Alice', status: 'online' as const },
  { id: 'p2', name: 'Bob', status: 'online' as const },
];

describe('CreateGroupModal', () => {
  it('renders input and peer list', () => {
    render(<CreateGroupModal isOpen={true} onClose={vi.fn()} peers={mockPeers} />);
    expect(screen.getByPlaceholderText(/Group Name/i)).toBeInTheDocument();
    expect(screen.getByText('Alice')).toBeInTheDocument();
    expect(screen.getByText('Bob')).toBeInTheDocument();
  });

  it('calls invoke create_group with selected members and name', async () => {
    const onClose = vi.fn();
    vi.mocked(invoke).mockResolvedValueOnce({
      id: 'group-1',
      name: 'Test Group',
      members: [
        { id: 'local', name: 'Local Device', status: 'online' },
        { id: 'p2', name: 'Bob', status: 'online' },
      ],
      createdAt: Date.now(),
    });
    render(<CreateGroupModal isOpen={true} onClose={onClose} peers={mockPeers} />);

    // Type group name
    fireEvent.change(screen.getByPlaceholderText(/Group Name/i), {
      target: { value: 'Test Group' },
    });

    // Select Bob
    const bobCheckbox = screen.getByLabelText('Bob');
    fireEvent.click(bobCheckbox);

    // Click Create
    fireEvent.click(screen.getByRole('button', { name: /Create/i }));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('create_group', {
        name: 'Test Group',
        members: ['p2'],
      });
      expect(onClose).toHaveBeenCalled();
    });
  });

  it('does not render when isOpen is false', () => {
    render(<CreateGroupModal isOpen={false} onClose={vi.fn()} peers={mockPeers} />);
    expect(screen.queryByPlaceholderText(/Group Name/i)).not.toBeInTheDocument();
  });
});

import { fireEvent, render, screen } from '@testing-library/react';
import { describe, it, expect, vi } from 'vitest';
import { MemberPanel } from './MemberPanel';
import { Peer } from '../../types/peer';
import '../../i18n/i18n';

const mockMembers: Peer[] = [
  { id: 'p1', name: 'Alice', status: 'online' },
  { id: 'p2', name: 'Bob', status: 'offline' },
];

describe('MemberPanel', () => {
  it('renders the list of members and their statuses', () => {
    render(<MemberPanel isOpen={true} members={mockMembers} />);

    expect(screen.getByText('Members (2)')).toBeInTheDocument();
    expect(screen.getByText('Alice')).toBeInTheDocument();
    expect(screen.getByText('Bob')).toBeInTheDocument();

    const onlineIndicators = screen.getAllByTestId('status-indicator');
    expect(onlineIndicators[0]).toHaveAttribute('aria-label', 'online');
    expect(onlineIndicators[1]).toHaveAttribute('aria-label', 'offline');
  });

  it('does not render when isOpen is false', () => {
    const { container } = render(<MemberPanel isOpen={false} members={mockMembers} />);
    expect(container.firstChild).toBeNull();
  });

  it('wires add, remove, and leave actions through the UI', () => {
    const onAddMembers = vi.fn();
    const onRemoveMember = vi.fn();
    const onLeaveGroup = vi.fn();
    const promptSpy = vi.spyOn(window, 'prompt').mockReturnValue('p3, p4');

    render(
      <MemberPanel
        isOpen={true}
        members={mockMembers}
        onAddMembers={onAddMembers}
        onRemoveMember={onRemoveMember}
        onLeaveGroup={onLeaveGroup}
      />
    );

    fireEvent.click(screen.getByRole('button', { name: 'Add Members' }));
    fireEvent.click(screen.getAllByRole('button', { name: 'Remove' })[0]);
    fireEvent.click(screen.getByRole('button', { name: 'Leave Group' }));

    expect(promptSpy).toHaveBeenCalledWith('Enter member IDs separated by commas');
    expect(onAddMembers).toHaveBeenCalledWith(['p3', 'p4']);
    expect(onRemoveMember).toHaveBeenCalledWith('p1');
    expect(onLeaveGroup).toHaveBeenCalled();

    promptSpy.mockRestore();
  });
});

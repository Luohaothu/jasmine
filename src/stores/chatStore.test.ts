import { describe, it, expect } from 'vitest';
import { useChatStore } from './chatStore';

describe('useChatStore', () => {
  it('initializes with empty messages', () => {
    useChatStore.setState({ messages: {} });
    const messages = useChatStore.getState().messages;
    expect(messages).toEqual({});
  });

  it('can add messages for a peer', () => {
    useChatStore.setState({ messages: {} });
    const peerId = 'peer-1';
    const message = {
      id: 'msg-1',
      senderId: 'local',
      receiverId: peerId,
      content: 'Hello',
      timestamp: 1234567890,
      status: 'sent' as const,
    };
    useChatStore.getState().addMessage(peerId, message);
    expect(useChatStore.getState().messages[peerId]).toEqual([message]);
  });

  it('can edit a message', () => {
    useChatStore.setState({ messages: {} });
    const peerId = 'peer-1';
    const message = {
      id: 'msg-1',
      senderId: 'local',
      receiverId: peerId,
      content: 'Hello',
      timestamp: 1234567890,
      status: 'sent' as const,
    };
    useChatStore.getState().addMessage(peerId, message);
    useChatStore.getState().editMessage(peerId, 'msg-1', 'Edited hello');

    const messages = useChatStore.getState().messages[peerId];
    expect(messages[0].content).toBe('Edited hello');
    expect(messages[0].editedAt).toBeDefined();
  });

  it('can delete a message (tombstone)', () => {
    useChatStore.setState({ messages: {} });
    const peerId = 'peer-1';
    const message = {
      id: 'msg-1',
      senderId: 'local',
      receiverId: peerId,
      content: 'Hello',
      timestamp: 1234567890,
      status: 'sent' as const,
    };
    useChatStore.getState().addMessage(peerId, message);
    useChatStore.getState().deleteMessage(peerId, 'msg-1');

    const messages = useChatStore.getState().messages[peerId];
    expect(messages[0].content).toBe('');
    expect(messages[0].isDeleted).toBe(true);
  });

  it('sorts clocked messages by causal vector order before timestamp', () => {
    useChatStore.setState({ messages: {} });
    const peerId = 'peer-1';

    const firstClockedByTimestamp = {
      id: 'msg-1',
      senderId: 'local',
      receiverId: peerId,
      content: 'first',
      timestamp: 5_000,
      status: 'sent' as const,
      vectorClock: { alice: 2 },
    };
    const secondClockedByTimestamp = {
      id: 'msg-2',
      senderId: 'local',
      receiverId: peerId,
      content: 'second',
      timestamp: 1_000,
      status: 'sent' as const,
      vectorClock: { alice: 1 },
    };

    useChatStore
      .getState()
      .setMessages(peerId, [firstClockedByTimestamp, secondClockedByTimestamp]);

    const ordered = useChatStore.getState().messages[peerId];
    expect(ordered?.[0].id).toBe('msg-2');
    expect(ordered?.[1].id).toBe('msg-1');
  });

  it('falls back to timestamp ordering for concurrent vector clocks', () => {
    useChatStore.setState({ messages: {} });
    const peerId = 'peer-1';

    const concurrentLeft = {
      id: 'msg-1',
      senderId: 'local',
      receiverId: peerId,
      content: 'left',
      timestamp: 2_000,
      status: 'sent' as const,
      vectorClock: { alice: 2, bob: 0 },
    };
    const concurrentRight = {
      id: 'msg-2',
      senderId: 'local',
      receiverId: peerId,
      content: 'right',
      timestamp: 1_000,
      status: 'sent' as const,
      vectorClock: { alice: 0, bob: 2 },
    };

    useChatStore.getState().setMessages(peerId, [concurrentLeft, concurrentRight]);

    const ordered = useChatStore.getState().messages[peerId];
    expect(ordered?.[0].id).toBe('msg-2');
    expect(ordered?.[1].id).toBe('msg-1');
  });

  it('keeps mixed clocked and unclocked messages in timestamp order', () => {
    useChatStore.setState({ messages: {} });
    const peerId = 'peer-1';

    const withClock = {
      id: 'msg-1',
      senderId: 'peer-1',
      receiverId: peerId,
      content: 'clocked but late',
      timestamp: 3_000,
      status: 'delivered' as const,
      vectorClock: { alice: 10 },
    };
    const noClockEarlier = {
      id: 'msg-2',
      senderId: 'peer-1',
      receiverId: peerId,
      content: 'legacy',
      timestamp: 1_000,
      status: 'delivered' as const,
    };
    const noClockLater = {
      id: 'msg-3',
      senderId: 'local',
      receiverId: peerId,
      content: 'newer',
      timestamp: 2_000,
      status: 'sent' as const,
    };

    useChatStore.getState().setMessages(peerId, [withClock, noClockEarlier, noClockLater]);

    const ordered = useChatStore.getState().messages[peerId];
    expect(ordered?.[0].id).toBe('msg-2');
    expect(ordered?.[1].id).toBe('msg-3');
    expect(ordered?.[2].id).toBe('msg-1');
  });
});

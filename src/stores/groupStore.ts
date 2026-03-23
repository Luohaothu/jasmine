import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { create } from 'zustand';
import { GroupMessage } from '../components/GroupChat/GroupChat';
import { Peer } from '../types/peer';

type MessageStatus = 'sent' | 'delivered' | 'failed';

interface BackendGroupMemberInfo {
  id?: string;
  deviceId?: string;
  device_id?: string;
  name: string;
  status?: string;
}

interface BackendGroupInfoResponse {
  id: string;
  name: string;
  members: BackendGroupMemberInfo[];
  createdAt?: number;
  created_at?: number;
}

interface GroupMessageReceivedEventPayload {
  groupId?: string;
  group_id?: string;
  id?: string;
  senderId?: string;
  sender_id?: string;
  senderName?: string;
  sender_name?: string;
  content?: string;
  timestamp?: number;
  encrypted?: boolean;
  isOwn?: boolean;
  is_own?: boolean;
  status?: string;
  isDeleted?: boolean;
  is_deleted?: boolean;
  editedAt?: number;
  edited_at?: number;
  replyToId?: string | null;
  reply_to_id?: string | null;
  replyToPreview?: string | null;
  reply_to_preview?: string | null;
}

type GroupEventPayload = Record<string, unknown>;

export interface Group {
  id: string;
  name: string;
  members: Peer[];
  messages: GroupMessage[];
  createdAt?: number;
}

/* eslint-disable no-unused-vars */
interface GroupStore {
  groups: Record<string, Group>;
  addGroup: (group: Group) => void;
  setGroups: (groups: Group[]) => void;
  removeGroup: (groupId: string) => void;
  addGroupMessage: (groupId: string, message: GroupMessage) => void;
  fetchGroups: () => Promise<Group[]>;
  fetchGroupInfo: (groupId: string) => Promise<Group>;
  createGroup: (name: string, memberIds: string[]) => Promise<Group>;
  sendGroupMessage: (groupId: string, content: string, replyToId?: string) => Promise<string>;
  addMembers: (groupId: string, memberIds: string[]) => Promise<Group>;
  removeMembers: (groupId: string, memberIds: string[]) => Promise<Group>;
  leaveGroup: (groupId: string) => Promise<void>;
}
/* eslint-enable no-unused-vars */

function normalizePeerStatus(status?: string): Peer['status'] {
  return status === 'online' ? 'online' : 'offline';
}

function memberId(member: BackendGroupMemberInfo): string {
  return member.id ?? member.deviceId ?? member.device_id ?? '';
}

function mapGroupMember(member: BackendGroupMemberInfo): Peer {
  return {
    id: memberId(member),
    name: member.name,
    status: normalizePeerStatus(member.status),
  };
}

function mapGroupResponse(response: BackendGroupInfoResponse, existing?: Group): Group {
  return {
    id: response.id,
    name: response.name,
    members: response.members.map(mapGroupMember),
    messages: existing?.messages ?? [],
    createdAt: response.createdAt ?? response.created_at ?? existing?.createdAt,
  };
}

function upsertGroup(group: Group, groups: Record<string, Group>): Record<string, Group> {
  const existing = groups[group.id];
  return {
    ...groups,
    [group.id]: {
      ...group,
      messages: group.messages.length > 0 ? group.messages : (existing?.messages ?? []),
    },
  };
}

function resolveReplyPreview(group: Group | undefined, replyToId?: string): string | undefined {
  if (!group || !replyToId) {
    return undefined;
  }

  const repliedMessage = group.messages.find((message) => message.id === replyToId);
  if (!repliedMessage) {
    return undefined;
  }

  return repliedMessage.content.length > 80
    ? `${repliedMessage.content.slice(0, 80)}...`
    : repliedMessage.content;
}

function optimisticGroupMessage(
  group: Group | undefined,
  messageId: string,
  content: string,
  replyToId?: string
): GroupMessage {
  return {
    id: messageId,
    senderId: 'me',
    senderName: 'Me',
    content,
    timestamp: Date.now(),
    encrypted: true,
    isOwn: true,
    status: 'sent',
    replyToId,
    replyToPreview: resolveReplyPreview(group, replyToId),
  };
}

function extractGroupId(payload: GroupEventPayload | null | undefined): string | undefined {
  if (!payload) {
    return undefined;
  }

  const groupId = payload.groupId ?? payload.group_id ?? payload.id;
  return typeof groupId === 'string' ? groupId : undefined;
}

function isGroupInfoResponse(payload: unknown): payload is BackendGroupInfoResponse {
  if (!payload || typeof payload !== 'object') {
    return false;
  }

  const maybeGroup = payload as Partial<BackendGroupInfoResponse>;
  return (
    typeof maybeGroup.id === 'string' &&
    typeof maybeGroup.name === 'string' &&
    Array.isArray(maybeGroup.members)
  );
}

function mapEventMessage(payload: GroupMessageReceivedEventPayload): GroupMessage | null {
  if (
    typeof payload.id !== 'string' ||
    typeof payload.content !== 'string' ||
    typeof payload.timestamp !== 'number'
  ) {
    return null;
  }

  const senderId =
    typeof payload.senderId === 'string'
      ? payload.senderId
      : typeof payload.sender_id === 'string'
        ? payload.sender_id
        : '';
  if (!senderId) {
    return null;
  }

  return {
    id: payload.id,
    senderId,
    senderName:
      typeof payload.senderName === 'string'
        ? payload.senderName
        : typeof payload.sender_name === 'string'
          ? payload.sender_name
          : undefined,
    content: payload.content,
    timestamp: payload.timestamp,
    encrypted: payload.encrypted,
    isOwn: payload.isOwn ?? payload.is_own ?? false,
    status: normalizeMessageStatus(payload.status),
    isDeleted: payload.isDeleted ?? payload.is_deleted,
    editedAt: payload.editedAt ?? payload.edited_at,
    replyToId: payload.replyToId ?? payload.reply_to_id ?? undefined,
    replyToPreview: payload.replyToPreview ?? payload.reply_to_preview ?? undefined,
  };
}

function normalizeMessageStatus(status?: string): MessageStatus | undefined {
  if (status === 'sent' || status === 'delivered' || status === 'failed') {
    return status;
  }

  return undefined;
}

function groupsRecordFromResponses(
  responses: BackendGroupInfoResponse[],
  existing: Record<string, Group>
): Record<string, Group> {
  return Object.fromEntries(
    responses.map((response) => {
      const current = existing[response.id];
      return [response.id, mapGroupResponse(response, current)];
    })
  );
}

export const useGroupStore = create<GroupStore>((set, get) => ({
  groups: {},
  addGroup: (group) =>
    set((state) => ({
      groups: upsertGroup(group, state.groups),
    })),
  setGroups: (groups) =>
    set(() => ({
      groups: Object.fromEntries(groups.map((group) => [group.id, group])),
    })),
  removeGroup: (groupId) =>
    set((state) => {
      const nextGroups = { ...state.groups };
      delete nextGroups[groupId];
      return { groups: nextGroups };
    }),
  addGroupMessage: (groupId, message) =>
    set((state) => {
      const group = state.groups[groupId];
      if (!group) {
        return state;
      }

      return {
        groups: {
          ...state.groups,
          [groupId]: {
            ...group,
            messages: [...group.messages, message],
          },
        },
      };
    }),
  fetchGroups: async () => {
    const responses = await invoke<BackendGroupInfoResponse[]>('list_groups');
    let groups: Group[] = [];
    set((state) => {
      const nextGroups = groupsRecordFromResponses(responses, state.groups);
      groups = Object.values(nextGroups);
      return { groups: nextGroups };
    });
    return groups;
  },
  fetchGroupInfo: async (groupId) => {
    const response = await invoke<unknown>('get_group_info', { groupId });
    if (!isGroupInfoResponse(response)) {
      const current = get().groups[groupId];
      if (current) {
        return current;
      }

      return {
        id: groupId,
        name: '',
        members: [],
        messages: [],
        createdAt: undefined,
      };
    }

    let group: Group = mapGroupResponse(response, get().groups[groupId]);
    set((state) => {
      group = mapGroupResponse(response, state.groups[groupId]);
      return {
        groups: upsertGroup(group, state.groups),
      };
    });
    return group;
  },
  createGroup: async (name, memberIds) => {
    const response = await invoke<BackendGroupInfoResponse>('create_group', {
      name,
      members: memberIds,
    });
    let group: Group = {
      id: response.id,
      name: response.name,
      members: response.members.map(mapGroupMember),
      messages: [],
      createdAt: response.createdAt ?? response.created_at,
    };
    set((state) => {
      group = mapGroupResponse(response, state.groups[response.id]);
      return {
        groups: upsertGroup(group, state.groups),
      };
    });
    return group;
  },
  sendGroupMessage: async (groupId, content, replyToId) => {
    const messageId = await invoke<string>('send_group_message', {
      groupId,
      content,
      replyToId: replyToId ?? null,
    });
    const group = get().groups[groupId];
    get().addGroupMessage(groupId, optimisticGroupMessage(group, messageId, content, replyToId));
    return messageId;
  },
  addMembers: async (groupId, memberIds) => {
    await invoke<void>('add_group_members', { groupId, memberIds });
    return get().fetchGroupInfo(groupId);
  },
  removeMembers: async (groupId, memberIds) => {
    await invoke<void>('remove_group_members', { groupId, memberIds });
    return get().fetchGroupInfo(groupId);
  },
  leaveGroup: async (groupId) => {
    await invoke<void>('leave_group', { groupId });
    get().removeGroup(groupId);
  },
}));

async function refreshGroupIfPossible(payload: GroupEventPayload): Promise<void> {
  if (isGroupInfoResponse(payload)) {
    const current = useGroupStore.getState().groups[payload.id];
    useGroupStore.getState().addGroup(mapGroupResponse(payload, current));
    return;
  }

  const groupId = extractGroupId(payload);
  if (!groupId) {
    return;
  }

  try {
    await useGroupStore.getState().fetchGroupInfo(groupId);
  } catch (error) {
    console.error('Failed to refresh group after event:', error);
  }
}

export async function setupGroupListeners(): Promise<() => void> {
  const unlistenGroupMessage = await listen<GroupMessageReceivedEventPayload>(
    'group-message-received',
    (event) => {
      const groupId = extractGroupId(event.payload as GroupEventPayload);
      const message = mapEventMessage(event.payload);
      if (!groupId || !message) {
        return;
      }

      useGroupStore.getState().addGroupMessage(groupId, message);
    }
  );

  const unlistenGroupCreated = await listen<GroupEventPayload>('group-created', (event) => {
    void refreshGroupIfPossible(event.payload);
  });

  const unlistenGroupMemberAdded = await listen<GroupEventPayload>(
    'group-member-added',
    (event) => {
      void refreshGroupIfPossible(event.payload);
    }
  );

  const unlistenGroupMemberRemoved = await listen<GroupEventPayload>(
    'group-member-removed',
    (event) => {
      void refreshGroupIfPossible(event.payload);
    }
  );

  const unlistenGroupUpdated = await listen<GroupEventPayload>('group-updated', (event) => {
    void refreshGroupIfPossible(event.payload);
  });

  return () => {
    unlistenGroupMessage();
    unlistenGroupCreated();
    unlistenGroupMemberAdded();
    unlistenGroupMemberRemoved();
    unlistenGroupUpdated();
  };
}

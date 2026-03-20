import { create } from "zustand";
import { Peer } from "../types/peer";
import { GroupMessage } from "../components/GroupChat/GroupChat";

export interface Group {
  id: string;
  name: string;
  members: Peer[];
  messages: GroupMessage[];
}

interface GroupStore {
  groups: Record<string, Group>;
  // eslint-disable-next-line no-unused-vars
  addGroup: (group: Group) => void;
  // eslint-disable-next-line no-unused-vars
  addGroupMessage: (groupId: string, message: GroupMessage) => void;
}

export const useGroupStore = create<GroupStore>((set) => ({
  groups: {
    // Adding a default mock group for testing/UI validation
    "g1": {
      id: "g1",
      name: "Mock Group",
      members: [
        { id: "p1", name: "Alice", status: "online" },
        { id: "p2", name: "Bob", status: "offline" }
      ],
      messages: [
        {
          id: "m1",
          senderId: "p1",
          senderName: "Alice",
          content: "Welcome to the group!",
          timestamp: Date.now() - 10000,
          isOwn: false
        }
      ]
    }
  },
  addGroup: (group) =>
    set((state) => ({
      groups: {
        ...state.groups,
        [group.id]: group,
      },
    })),
  addGroupMessage: (groupId, message) =>
    set((state) => {
      const group = state.groups[groupId];
      if (!group) return state;
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
}));

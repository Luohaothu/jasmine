import { create } from "zustand";

export interface ChatMessage {
  id: string;
  senderId: string;
  receiverId: string;
  content: string;
  timestamp: number;
  status: "sent" | "delivered" | "failed";
}

interface ChatStore {
  messages: Record<string, ChatMessage[]>;
  // eslint-disable-next-line no-unused-vars
  addMessage: (peerId: string, message: ChatMessage) => void;
  // eslint-disable-next-line no-unused-vars
  setMessages: (peerId: string, messages: ChatMessage[]) => void;
}

export const useChatStore = create<ChatStore>((set) => ({
  messages: {},
  addMessage: (peerId, message) =>
    set((state) => ({
      messages: {
        ...state.messages,
        [peerId]: [...(state.messages[peerId] || []), message],
      },
    })),
  setMessages: (peerId, messages) =>
    set((state) => ({
      messages: {
        ...state.messages,
        [peerId]: messages,
      },
    })),
}));

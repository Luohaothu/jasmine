import React from "react";
import { useParams } from "react-router-dom";
import { invoke } from "@tauri-apps/api/core";
import { GroupChat } from "./GroupChat";
import { useGroupStore } from "../../stores/groupStore";

export const GroupChatWrapper: React.FC = () => {
  const { groupId } = useParams<{ groupId: string }>();
  const groups = useGroupStore((state) => state.groups);
  const addGroupMessage = useGroupStore((state) => state.addGroupMessage);

  if (!groupId) return <div className="placeholder-view">Invalid Group ID</div>;

  const group = groups[groupId];

  if (!group) {
    return <div className="placeholder-view">Group not found</div>;
  }

  const handleSendMessage = async (content: string) => {
    try {
      await invoke("send_group_message", {
        groupId,
        content,
      });
      // Optimistic update for UI feel, in reality you'd listen for an event
      addGroupMessage(groupId, {
        id: Date.now().toString(),
        senderId: "me",
        senderName: "Me",
        content,
        timestamp: Date.now(),
        isOwn: true,
      });
    } catch {
      // Ignored intentionally for now
    }
  };

  return (
    <GroupChat
      groupName={group.name}
      members={group.members}
      messages={group.messages}
      onSendMessage={handleSendMessage}
    />
  );
};

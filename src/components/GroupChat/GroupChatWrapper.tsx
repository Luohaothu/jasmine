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

  const handleSendMessage = async (content: string, replyToId?: string) => {
    try {
      const messageId = await invoke<string>("send_group_message", {
        groupId,
        content,
        replyToId: replyToId || null,
      });

      let replyToPreview: string | undefined;
      if (replyToId) {
        const repliedMsg = group.messages.find(m => m.id === replyToId);
        if (repliedMsg) {
          replyToPreview = repliedMsg.content.length > 80 
            ? repliedMsg.content.slice(0, 80) + '…' 
            : repliedMsg.content;
        }
      }

      addGroupMessage(groupId, {
        id: messageId,
        senderId: "me",
        senderName: "Me",
        content,
        timestamp: Date.now(),
        isOwn: true,
        replyToId,
        replyToPreview,
      });
    } catch (error) {
      console.error("Failed to send group message:", error);
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

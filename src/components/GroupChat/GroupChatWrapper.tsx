import React, { useEffect } from 'react';
import { useParams } from 'react-router-dom';
import { GroupChat } from './GroupChat';
import { useGroupStore } from '../../stores/groupStore';

export const GroupChatWrapper: React.FC = () => {
  const { groupId } = useParams<{ groupId: string }>();
  const groups = useGroupStore((state) => state.groups);
  const fetchGroupInfo = useGroupStore((state) => state.fetchGroupInfo);
  const sendGroupMessage = useGroupStore((state) => state.sendGroupMessage);
  const addMembers = useGroupStore((state) => state.addMembers);
  const removeMembers = useGroupStore((state) => state.removeMembers);
  const leaveGroup = useGroupStore((state) => state.leaveGroup);

  useEffect(() => {
    if (!groupId) {
      return;
    }

    void fetchGroupInfo(groupId).catch((error) => {
      console.error('Failed to fetch group info:', error);
    });
  }, [fetchGroupInfo, groupId]);

  if (!groupId) return <div className="placeholder-view">Invalid Group ID</div>;

  const group = groups[groupId];

  if (!group) {
    return <div className="placeholder-view">Group not found</div>;
  }

  const handleSendMessage = async (content: string, replyToId?: string) => {
    try {
      await sendGroupMessage(groupId, content, replyToId);
    } catch (error) {
      console.error('Failed to send group message:', error);
    }
  };

  const handleAddMembers = async (memberIds: string[]) => {
    try {
      await addMembers(groupId, memberIds);
    } catch (error) {
      console.error('Failed to add group members:', error);
    }
  };

  const handleRemoveMember = async (memberId: string) => {
    try {
      await removeMembers(groupId, [memberId]);
    } catch (error) {
      console.error('Failed to remove group member:', error);
    }
  };

  const handleLeaveGroup = async () => {
    try {
      await leaveGroup(groupId);
    } catch (error) {
      console.error('Failed to leave group:', error);
    }
  };

  return (
    <GroupChat
      groupName={group.name}
      members={group.members}
      messages={group.messages}
      onSendMessage={handleSendMessage}
      onAddMembers={handleAddMembers}
      onRemoveMember={handleRemoveMember}
      onLeaveGroup={handleLeaveGroup}
    />
  );
};

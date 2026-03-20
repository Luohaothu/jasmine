import { createBrowserRouter, Outlet } from "react-router-dom";
import { DeviceList } from "./components/DeviceList";
import { ChatView } from "./components/ChatView/ChatView";
import Settings from "./components/Settings/Settings";
import { GroupChatWrapper } from "./components/GroupChat/GroupChatWrapper";
import { FileTransferPanel } from "./components/FileTransfer/FileTransferPanel";
import { FileReceiveDialog } from "./components/FileTransfer/FileReceiveDialog";
import "./App.css";

export const AppShell = () => {
  return (
    <div className="app-shell" data-testid="app-shell">
      <DeviceList />
      <main className="main-content">
        <Outlet />
      </main>
      <FileTransferPanel />
      <FileReceiveDialog />
    </div>
  );
};

const WelcomePlaceholder = () => {
  return <div className="placeholder-view">Select a chat to start messaging</div>;
};

export const appRouter = createBrowserRouter([
  {
    path: "/",
    element: <AppShell />,
    children: [
      { index: true, element: <WelcomePlaceholder /> },
      { path: "chat/:peerId", element: <ChatView /> },
      { path: "group/:groupId", element: <GroupChatWrapper /> },
      { path: "settings", element: <Settings /> },
    ],
  },
]);

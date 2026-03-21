import { useEffect } from "react";
import { createBrowserRouter, Outlet } from "react-router-dom";
import { DeviceList } from "./components/DeviceList";
import { ChatView } from "./components/ChatView/ChatView";
import Settings from "./components/Settings/Settings";
import { GroupChatWrapper } from "./components/GroupChat/GroupChatWrapper";
import { FileTransferPanel } from "./components/FileTransfer/FileTransferPanel";
import { FileReceiveDialog } from "./components/FileTransfer/FileReceiveDialog";
import { FolderTransferPanel } from "./components/FolderTransfer/FolderTransferPanel";
import { FolderReceiveDialog } from "./components/FolderTransfer/FolderReceiveDialog";
import { setupChatEventListeners } from "./stores/chatStore";
import { setupFolderTransferListeners } from "./stores/folderTransferStore";
import { setupTransferListeners } from "./stores/transferStore";
import "./App.css";

export const AppShell = () => {
  useEffect(() => {
    const cleanups: Array<() => void> = [];
    let disposed = false;

    const registerListeners = async () => {
      for (const setup of [setupTransferListeners, setupFolderTransferListeners, setupChatEventListeners]) {
        try {
          const cleanup = await setup();
          if (disposed) {
            cleanup();
          } else {
            cleanups.push(cleanup);
          }
        } catch (error) {
          console.error("Listener setup failed:", error);
        }
      }
    };

    registerListeners().catch((error) => {
      console.error("Failed to register listeners:", error);
    });

    return () => {
      disposed = true;
      cleanups.forEach((cleanup) => cleanup());
    };
  }, []);

  return (
    <div className="app-shell" data-testid="app-shell">
      <DeviceList />
      <main className="main-content">
        <Outlet />
      </main>
      <FileTransferPanel />
      <FileReceiveDialog />
      <FolderTransferPanel />
      <FolderReceiveDialog />
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

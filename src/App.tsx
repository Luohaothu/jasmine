import { RouterProvider } from 'react-router-dom';
import { appRouter } from './router';
import { useNotifications } from './hooks/useNotifications';
import './App.css';

function App() {
  useNotifications();

  return <RouterProvider router={appRouter} />;
}

export default App;

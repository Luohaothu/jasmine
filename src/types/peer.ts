export interface Peer {
  id: string;
  name: string;
  status: 'online' | 'offline' | 'incompatible';
  warning?: string;
}

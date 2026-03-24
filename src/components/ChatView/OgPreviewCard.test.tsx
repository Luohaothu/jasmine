import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import '../../i18n/i18n';
import { OgPreviewCard } from './OgPreviewCard';

const { listenMock, listeners } = vi.hoisted(() => ({
  listenMock: vi.fn(),
  listeners: new Map<string, Function>(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: listenMock,
}));

describe('OgPreviewCard', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    listeners.clear();
    listenMock.mockImplementation(async (eventName: string, callback: Function) => {
      listeners.set(eventName, callback);
      return () => {
        listeners.delete(eventName);
      };
    });
  });

  it('shows loading skeleton initially', () => {
    vi.mocked(invoke).mockReturnValue(new Promise(() => {}));
    render(<OgPreviewCard url="https://example.com" />);

    expect(screen.getByTestId('og-preview-loading')).toBeInTheDocument();
  });

  it('renders card when data is returned', async () => {
    vi.mocked(invoke).mockResolvedValue({
      url: 'https://example.com',
      title: 'Test Title',
      description: 'Test Description',
      image_url: 'https://example.com/image.png',
      site_name: 'Test Site',
    });

    render(<OgPreviewCard url="https://example.com" />);

    await waitFor(() => {
      expect(screen.getByTestId('og-preview-card')).toBeInTheDocument();
    });

    expect(screen.getByText('Test Title')).toBeInTheDocument();
    expect(screen.getByText('Test Description')).toBeInTheDocument();
    expect(screen.getByText('Test Site')).toBeInTheDocument();
    expect(screen.getByAltText('Test Title')).toHaveAttribute(
      'src',
      'https://example.com/image.png'
    );
  });

  it('renders null when invocation fails', async () => {
    vi.mocked(invoke).mockRejectedValue(new Error('Network error'));

    const { container } = render(<OgPreviewCard url="https://example.com" />);

    await waitFor(() => {
      expect(screen.queryByTestId('og-preview-loading')).not.toBeInTheDocument();
    });

    expect(screen.queryByTestId('og-preview-card')).not.toBeInTheDocument();
    expect(container).toBeEmptyDOMElement();
  });

  it('renders null when empty metadata is returned', async () => {
    vi.mocked(invoke).mockResolvedValue({
      url: 'https://example.com',
      title: null,
      description: null,
      image_url: null,
      site_name: null,
    });

    const { container } = render(<OgPreviewCard url="https://example.com" />);

    await waitFor(() => {
      expect(screen.queryByTestId('og-preview-loading')).not.toBeInTheDocument();
    });

    expect(screen.queryByTestId('og-preview-card')).not.toBeInTheDocument();
    expect(container).toBeEmptyDOMElement();
  });

  it('uses the localized fallback alt text when the preview image has no title', async () => {
    vi.mocked(invoke).mockResolvedValue({
      url: 'https://example.com',
      title: null,
      description: 'Test Description',
      image_url: 'https://example.com/image.png',
      site_name: 'Test Site',
    });

    render(<OgPreviewCard url="https://example.com" />);

    await waitFor(() => {
      expect(screen.getByTestId('og-preview-card')).toBeInTheDocument();
    });

    expect(screen.getByAltText('Preview')).toHaveAttribute('src', 'https://example.com/image.png');
  });

  it('refreshes preview when og:updated emits matching metadata', async () => {
    vi.mocked(invoke).mockResolvedValue({
      url: 'https://example.com',
      title: 'Stale Title',
      description: 'Stale Description',
      image_url: null,
      site_name: 'Example Site',
    });

    render(<OgPreviewCard url="https://example.com" />);

    await waitFor(() => {
      expect(screen.getByText('Stale Title')).toBeInTheDocument();
    });

    listeners.get('og:updated')?.({
      payload: {
        url: 'https://example.com',
        metadata: {
          url: 'https://example.com',
          title: 'Fresh Title',
          description: 'Fresh Description',
          image_url: null,
          site_name: 'Example Site',
        },
      },
    });

    await waitFor(() => {
      expect(screen.getByText('Fresh Title')).toBeInTheDocument();
    });

    expect(screen.queryByText('Stale Title')).not.toBeInTheDocument();
  });
});

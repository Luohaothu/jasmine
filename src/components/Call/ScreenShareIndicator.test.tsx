import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import i18n from '../../i18n/i18n';
import { ScreenShareIndicator } from './ScreenShareIndicator';

describe('ScreenShareIndicator', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('en');
  });

  it('renders correctly and handles stop click', () => {
    const onStopSharing = vi.fn();
    render(<ScreenShareIndicator onStopSharing={onStopSharing} />);

    expect(screen.getByTestId('screen-share-indicator')).toBeInTheDocument();
    expect(screen.getByText('You are sharing your screen')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('btn-stop-share'));
    expect(onStopSharing).toHaveBeenCalled();
  });
});

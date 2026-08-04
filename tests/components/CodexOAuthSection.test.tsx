import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { CodexOAuthSection } from "@/components/providers/forms/CodexOAuthSection";
import { AuthCenterPanel } from "@/components/settings/AuthCenterPanel";

const mocks = vi.hoisted(() => ({
  useCodexOauth: vi.fn(),
  renderAccountQuota: vi.fn(),
}));

vi.mock("@/components/providers/forms/hooks/useCodexOauth", () => ({
  useCodexOauth: mocks.useCodexOauth,
}));

vi.mock("@/components/CodexOauthAccountQuota", () => ({
  default: ({ accountId }: { accountId: string }) => {
    mocks.renderAccountQuota(accountId);
    return <div data-testid="account-quota">{accountId}</div>;
  },
}));

describe("CodexOAuthSection", () => {
  beforeEach(() => {
    mocks.useCodexOauth.mockReturnValue({
      accounts: [{ id: "account-1", login: "user@example.com" }],
      defaultAccountId: "account-1",
      hasAnyAccount: true,
      pollingState: "idle",
      deviceCode: null,
      error: null,
      isPolling: false,
      isAddingAccount: false,
      isRemovingAccount: false,
      isSettingDefaultAccount: false,
      addAccount: vi.fn(),
      removeAccount: vi.fn(),
      setDefaultAccount: vi.fn(),
      cancelAuth: vi.fn(),
      logout: vi.fn(),
    });
  });

  it("does not render account quota by default", () => {
    render(<CodexOAuthSection />);
    expect(mocks.renderAccountQuota).not.toHaveBeenCalled();
  });

  it("renders account quota in Auth Center", () => {
    render(<AuthCenterPanel />);
    expect(screen.getByTestId("account-quota")).toHaveTextContent("account-1");
  });
});

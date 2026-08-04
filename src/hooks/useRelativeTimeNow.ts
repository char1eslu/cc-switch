import { useEffect, useState } from "react";

const RELATIVE_TIME_REFRESH_INTERVAL = 30_000;

export function useRelativeTimeNow(enabled: boolean): number {
  const [now, setNow] = useState(Date.now);

  useEffect(() => {
    if (!enabled) return;

    setNow(Date.now());
    const interval = window.setInterval(
      () => setNow(Date.now()),
      RELATIVE_TIME_REFRESH_INTERVAL,
    );

    return () => window.clearInterval(interval);
  }, [enabled]);

  return now;
}

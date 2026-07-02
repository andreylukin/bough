// Phone-width detection for the responsive shell. One breakpoint, shared everywhere:
// below it the rails become drawers and the conversation takes the full width.
import { useEffect, useState } from "react";

const QUERY = "(max-width: 700px)";

export function useIsMobile(): boolean {
  const [mobile, setMobile] = useState(() => window.matchMedia(QUERY).matches);
  useEffect(() => {
    const mq = window.matchMedia(QUERY);
    const onChange = () => setMobile(mq.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);
  return mobile;
}

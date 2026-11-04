"use client";
import { usePathname } from "next/navigation";
import { FeedbackProvider } from "./Feedback";
import { Shell } from "./Shell";
import { OvProvider } from "@/lib/store";
export function AppRoot({ children }: { children: React.ReactNode }) {
  const path = usePathname();
  if (path === "/login" || path === "/login/") return <>{children}</>;
  return <OvProvider><FeedbackProvider><Shell>{children}</Shell></FeedbackProvider></OvProvider>;
}

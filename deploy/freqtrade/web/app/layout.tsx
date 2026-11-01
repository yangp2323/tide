import type { Metadata, Viewport } from "next";
import "./globals.css";
import "./ui.css";
import { FeedbackProvider } from "@/components/Feedback";
import { Shell } from "@/components/Shell";
import { OvProvider } from "@/lib/store";

export const metadata: Metadata = {
  title: "Freqtrade 管理后台",
  description: "Bybit 量化交易管理后台：持仓、交易记录、策略、AI 交易与风控",
  robots: { index: false, follow: false },
};

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  themeColor: "#0f1722",
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="zh-CN">
      <body>
        <OvProvider>
          <FeedbackProvider>
            <Shell>{children}</Shell>
          </FeedbackProvider>
        </OvProvider>
      </body>
    </html>
  );
}

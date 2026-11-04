//! Starter notes for the knowledge base, written from public strategy material. The sources
//! are mostly articles and vendor pages, not validated studies, and each note says so.

/// (title, content, source)
pub const SEEDS: &[(&str, &str, &str)] = &[
    (
        "如何使用这些笔记",
        "这些笔记来自公开资料，多为博客、教程和厂商文档，缺少严格的样本外验证，只是启发，不是规则。\n\
         - 不要因为某条笔记“匹配”就开仓；没有正期望的机会就观望。\n\
         - 优先级低于系统给出的风控限制、动作格式和你对数据的直接判断。\n\
         - 回测数字往往没算手续费、滑点和资金费，实盘会更差；账户样本很小时不要过度解读。\n\
         - 分析里请说明参考或否决了哪条笔记，以及理由。",
        "",
    ),
    (
        "趋势跟随与 ATR 追踪止损",
        "趋势跟随在趋势行情里赚钱，在震荡行情里被来回打止损。\n\
         - 常见的“吊灯止损”设置是回看约 22 根 K 线、止损距最高价约 3×ATR；倍数越大止损越慢，更能骑住趋势、假信号更少，但回吐的利润也更多。\n\
         - 证据是混合的：一篇覆盖 150 多个币对、2022–2024 年样本外的研究认为自适应的 ATR 追踪止损有帮助；也有针对 BTC 日线的博客回测认为追踪止损反而降低收益（且未计滑点）。\n\
         - 做法上：浮盈刚出现时不要把止损收得太紧，只在结构被破坏（关键高低点、均线）或波动明显收缩时收紧；收紧的理由要能说清。",
        "https://arxiv.org/pdf/2602.11708.pdf ; https://www.mql5.com/en/market/product/185703 ; https://mohit-sharma-website.pages.dev/blogs/btc-ema20-sma50-strategy",
    ),
    (
        "唐奇安通道突破（海龟）与假突破过滤",
        "经典海龟：突破 20 日高低点入场、反向 10 日高低点出场；慢速版用 55 日。止损约 2×ATR，单笔风险约占权益 1%。\n\
         - 这类系统胜率通常不高（常见说法是 30%–40%，但没有可靠数据来源），靠盈亏比赚钱；连续几次小亏不等于失效。\n\
         - 过滤假突破的常见办法：突破方向与更高周期趋势一致（例如日线 200 EMA 的方向）；ADX 高于约 25；成交量放大；等回踩确认再进（少交易，也会错过一些）。\n\
         - 高波动环境里假突破更常见；突破后很快收回通道内，是离场信号，不要加仓摊平。",
        "https://blog.traderspost.io/article/donchian-channel-trading-strategies ; https://trendspider.com/learning-center/donchian-channel-trading-strategies/ ; https://www.luxalgo.com/library/concept/donchian-trend-rules/",
    ),
    (
        "判断趋势行情还是震荡行情",
        "先判断行情状态，再决定用什么打法。\n\
         - ADX 低于约 20 通常表示没有明显趋势，高于 25–30 表示趋势成形；布林带宽度低于其 50 根均值的约 0.8 倍偏向震荡。两个信号同时一致时更可靠。\n\
         - 这些指标都有滞后，阈值只是常用经验值。\n\
         - 震荡市里趋势类开仓容易被来回打：可以观望，或把仓位（risk_pct）调小，而不是硬做。",
        "https://equitiesindia.com/glossary/market-regime-detection ; https://de.tradingview.com/script/Syv6Sd57-BB-Breakout-Momentum-Reversion-Strategies",
    ),
    (
        "资金费率与持仓量（永续合约）",
        "资金费率是多空之间的周期性付费，也反映仓位拥挤程度。\n\
         - 持续偏高的正费率：多头拥挤，付费维持多头；明显为负：空头拥挤。\n\
         - 持仓量（OI）上升且费率极端：拥挤还在加码；OI 下降：平仓踩踏可能已经开始。\n\
         - 极端读数只说明“脆弱”，不说明“时机”：必须有价格行为确认（例如跌破关键位、放量失败的突破）才有意义。常被引用的阈值（例如 8 小时费率高于 0.05%）只是个别作者的经验，没有验证。\n\
         - 持仓成本：做多在高正费率下持仓会被持续扣费，做空则相反，计算预期收益时要算进去。",
        "https://www.luxalgo.com/library/concept/funding-rate/ ; https://www.tradermath.org/courses/introduction-to-digital-assets/funding-rates-and-perpetual-futures ; https://www.sharpe.ai/futures/funding-rate",
    ),
    (
        "仓位与风险管理",
        "- 按波动率定仓位：波动越大，同样风险下的仓位越小（这正是 风险÷止损距离 的含义）；山寨币波动常是 BTC 的数倍。\n\
         - 加密货币里单笔风险常用 0.5%–1%，比传统的 2% 更保守。\n\
         - 凯利公式的输入（胜率、盈亏比）很难估准，常用四分之一到一半凯利；没有可靠统计时宁可小。\n\
         - 高相关的币（例如 BTC、ETH、SOL 同向）更像同一笔大仓位，组合风险比单笔相加更集中。\n\
         - 单笔仓位之外还要有账户层面的约束：当日亏损上限、回撤熔断、连亏暂停，系统已经设置。",
        "https://www.altrady.com/blog/risk-management/kelly-criterion-crypto-position-sizing ; https://www.spark.money/glossary/position-sizing",
    ),
    (
        "本账户的历史观察（样本很小）",
        "来自本系统早期 30 笔已平仓交易（11 月初），样本小，只作提醒：\n\
         - 由交易所止损单（含移动止损）退出的 16 笔合计约 +10.3 USDT；由 AI 主动平仓的 14 笔合计约 −8.5 USDT。\n\
         - 同一个币在短时间内反复开仓又平仓（例如 XRP 一晚上多次）基本是净亏损，手续费和滑点会吃掉小波动的收益；单日交易笔数很多的那天表现最差。\n\
         - 因此：除非原来的开仓论点已经被明确证伪，否则让止损和止盈单去管理出场；平仓前先说明是什么失效了；刚平掉的币不要马上反向或重新开。",
        "本系统交易记录",
    ),
];

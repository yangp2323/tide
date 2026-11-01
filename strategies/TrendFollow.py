# 4 小时趋势跟踪。现货只做多；合约可加杠杆，可选做空（控制台里设置）
# 做多：价格在 EMA200 上方，EMA20 上穿 EMA50，ADX > 20；EMA20 下穿 EMA50 平仓
# 做空：价格在 EMA200 下方，EMA20 下穿 EMA50，ADX > 20；EMA20 上穿 EMA50 平仓
# 止损和移动止盈都按「价格」算：不论几倍杠杆，价格反向走 8% 止损
# AI 模式（控制台里开启）：均线信号停用，由大模型通过 forceenter/forceexit 开平仓，止损和移动止盈照旧
from pandas import DataFrame
import talib.abstract as ta
from freqtrade.strategy import IStrategy, IntParameter
import freqtrade.vendor.qtpylib.indicators as qtpylib

PRICE_STOP = 0.08            # 价格反向 8% 止损
PRICE_TRAIL = 0.03           # 移动止盈：从最高点回撤 3%
PRICE_TRAIL_START = 0.06     # 盈利 6% 后启动移动止盈


class TrendFollow(IStrategy):
    INTERFACE_VERSION = 3
    timeframe = "4h"
    can_short = False

    minimal_roi = {"0": 100}
    stoploss = -PRICE_STOP
    trailing_stop = True
    trailing_stop_positive = PRICE_TRAIL
    trailing_stop_positive_offset = PRICE_TRAIL_START
    trailing_only_offset_is_reached = True
    order_types = {
        "entry": "limit",
        "exit": "limit",
        "stoploss": "market",
        "stoploss_on_exchange": False,
    }

    startup_candle_count = 220
    process_only_new_candles = True

    adx_min = IntParameter(15, 30, default=20, space="buy")

    def __init__(self, config: dict) -> None:
        super().__init__(config)
        futures = config.get("trading_mode") == "futures"
        self.lev = max(1, int(config.get("panel_leverage", 1))) if futures else 1
        self.can_short = futures and bool(config.get("panel_short", False))
        self.ai = bool(config.get("panel_ai", False))
        # Freqtrade 的止损和止盈按保证金收益率算（会除以杠杆），这里乘回去，让它们表示价格波动
        self.stoploss = -PRICE_STOP * self.lev
        self.trailing_stop_positive = PRICE_TRAIL * self.lev
        self.trailing_stop_positive_offset = PRICE_TRAIL_START * self.lev
        # 合约的止损单挂在 Bybit 上，机器人宕机也能止损；现货 Freqtrade 不支持
        self.order_types = {**self.order_types, "stoploss_on_exchange": futures}

    def leverage(self, pair, current_time, current_rate, proposed_leverage, max_leverage,
                 entry_tag, side, **kwargs) -> float:
        return min(self.lev, max_leverage)

    def populate_indicators(self, dataframe: DataFrame, metadata: dict) -> DataFrame:
        dataframe["ema20"] = ta.EMA(dataframe, timeperiod=20)
        dataframe["ema50"] = ta.EMA(dataframe, timeperiod=50)
        dataframe["ema200"] = ta.EMA(dataframe, timeperiod=200)
        dataframe["adx"] = ta.ADX(dataframe, timeperiod=14)
        # 给 AI 看的辅助指标
        dataframe["rsi"] = ta.RSI(dataframe, timeperiod=14)
        dataframe["atr"] = ta.ATR(dataframe, timeperiod=14)
        return dataframe

    def populate_entry_trend(self, dataframe: DataFrame, metadata: dict) -> DataFrame:
        if self.ai:
            return dataframe
        trending = (dataframe["adx"] > self.adx_min.value) & (dataframe["volume"] > 0)
        dataframe.loc[
            (dataframe["close"] > dataframe["ema200"])
            & qtpylib.crossed_above(dataframe["ema20"], dataframe["ema50"])
            & trending,
            "enter_long",
        ] = 1
        dataframe.loc[
            (dataframe["close"] < dataframe["ema200"])
            & qtpylib.crossed_below(dataframe["ema20"], dataframe["ema50"])
            & trending,
            "enter_short",
        ] = 1
        return dataframe

    def populate_exit_trend(self, dataframe: DataFrame, metadata: dict) -> DataFrame:
        if self.ai:
            return dataframe
        has_volume = dataframe["volume"] > 0
        dataframe.loc[qtpylib.crossed_below(dataframe["ema20"], dataframe["ema50"]) & has_volume, "exit_long"] = 1
        dataframe.loc[qtpylib.crossed_above(dataframe["ema20"], dataframe["ema50"]) & has_volume, "exit_short"] = 1
        return dataframe

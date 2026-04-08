namespace FindX.Core.Pinyin;

/// <summary>
/// CJK 统一汉字 → 拼音映射表。覆盖 CJK 基本区 (U+4E00-U+9FFF) 常用字，
/// 含多音字（返回所有读音）。数据精简为无声调纯拼音。
/// </summary>
public static class PinyinTable
{
    private static readonly Dictionary<char, string[]> MultiReadings = new();
    private static readonly string?[] SingleReadings = new string?[0x9FFF - 0x4E00 + 1];
    private static bool _initialized;

    public static void EnsureInitialized()
    {
        if (_initialized) return;
        lock (MultiReadings)
        {
            if (_initialized) return;
            BuildTable();
            _initialized = true;
        }
    }

    public static string[]? GetReadings(char ch)
    {
        EnsureInitialized();
        if (MultiReadings.TryGetValue(ch, out var multi))
            return multi;
        if (ch >= '\u4E00' && ch <= '\u9FFF')
        {
            var s = SingleReadings[ch - '\u4E00'];
            return s != null ? [s] : null;
        }
        return null;
    }

    public static string? GetPrimaryReading(char ch)
    {
        var r = GetReadings(ch);
        return r is { Length: > 0 } ? r[0] : null;
    }

    public static bool IsCjk(char ch) => ch >= '\u4E00' && ch <= '\u9FFF';

    public static bool NameContainsCjk(string name)
    {
        foreach (var c in name)
        {
            if (IsCjk(c)) return true;
        }

        return false;
    }

    public static string[] GetPinyinSequence(string text)
    {
        EnsureInitialized();
        var result = new List<string>();
        foreach (var ch in text)
        {
            var primary = GetPrimaryReading(ch);
            result.Add(primary ?? ch.ToString().ToLowerInvariant());
        }
        return result.ToArray();
    }

    public static string GetInitials(string text)
    {
        EnsureInitialized();
        var sb = new System.Text.StringBuilder(text.Length);
        foreach (var ch in text)
        {
            var primary = GetPrimaryReading(ch);
            if (primary is { Length: > 0 })
                sb.Append(primary[0]);
            else if (char.IsAsciiLetterOrDigit(ch))
                sb.Append(char.ToLowerInvariant(ch));
        }
        return sb.ToString();
    }

    private static void Set(char ch, string py)
    {
        if (ch >= '\u4E00' && ch <= '\u9FFF')
            SingleReadings[ch - '\u4E00'] = py;
    }

    private static void SetMulti(char ch, params string[] pys)
    {
        MultiReadings[ch] = pys;
        if (ch >= '\u4E00' && ch <= '\u9FFF')
            SingleReadings[ch - '\u4E00'] = pys[0];
    }

    private static void BuildTable()
    {
        SetMulti('长', "chang", "zhang");
        SetMulti('重', "zhong", "chong");
        SetMulti('中', "zhong", "zhong");
        SetMulti('行', "xing", "hang");
        SetMulti('了', "le", "liao");
        SetMulti('地', "di", "de");
        SetMulti('大', "da", "dai");
        SetMulti('会', "hui", "kuai");
        SetMulti('发', "fa", "fa");
        SetMulti('得', "de", "dei", "de");
        SetMulti('还', "hai", "huan");
        SetMulti('要', "yao", "yao");
        SetMulti('没', "mei", "mo");
        SetMulti('好', "hao", "hao");
        SetMulti('看', "kan", "kan");
        SetMulti('说', "shuo", "shui");
        SetMulti('为', "wei", "wei");
        SetMulti('都', "dou", "du");
        SetMulti('乐', "le", "yue");
        SetMulti('相', "xiang", "xiang");
        SetMulti('数', "shu", "shuo", "shu");
        SetMulti('调', "diao", "tiao");
        SetMulti('干', "gan", "gan");
        SetMulti('量', "liang", "liang");
        SetMulti('强', "qiang", "jiang", "qiang");
        SetMulti('差', "cha", "chai", "ci");
        SetMulti('传', "chuan", "zhuan");
        SetMulti('教', "jiao", "jiao");
        SetMulti('切', "qie", "qie");
        SetMulti('落', "luo", "la", "lao");
        SetMulti('难', "nan", "nan");
        SetMulti('朝', "chao", "zhao");
        SetMulti('曾', "ceng", "zeng");
        SetMulti('模', "mo", "mu");
        SetMulti('处', "chu", "chu");
        SetMulti('空', "kong", "kong");
        SetMulti('乘', "cheng", "sheng");
        SetMulti('藏', "cang", "zang");
        SetMulti('薄', "bao", "bo", "bo");
        SetMulti('弹', "dan", "tan");
        SetMulti('担', "dan", "dan");
        SetMulti('角', "jiao", "jue");
        SetMulti('降', "jiang", "xiang");
        SetMulti('系', "xi", "ji");
        SetMulti('率', "lv", "shuai");
        SetMulti('供', "gong", "gong");
        SetMulti('应', "ying", "ying");
        SetMulti('省', "sheng", "xing");
        SetMulti('参', "can", "shen", "cen");
        SetMulti('卷', "juan", "juan");
        SetMulti('便', "bian", "pian");
        SetMulti('兴', "xing", "xing");
        SetMulti('似', "si", "shi");
        SetMulti('将', "jiang", "jiang");
        SetMulti('奇', "qi", "ji");
        SetMulti('解', "jie", "jie", "xie");
        SetMulti('属', "shu", "zhu");
        SetMulti('种', "zhong", "zhong");
        SetMulti('背', "bei", "bei");
        SetMulti('弄', "nong", "long");
        SetMulti('场', "chang", "chang");
        SetMulti('宁', "ning", "ning");
        SetMulti('假', "jia", "jia");
        SetMulti('仔', "zi", "zai", "zi");
        SetMulti('恶', "e", "wu", "e");
        SetMulti('和', "he", "huo", "hu");
        SetMulti('磨', "mo", "mo");
        SetMulti('当', "dang", "dang");

        BuildBulkMappings();
    }

    private static void BuildBulkMappings()
    {
        var map = new (int start, int end, string[] readings)[]
        {
            (0x4E00, 0x4E00, new[]{"yi"}),         // 一
            (0x4E01, 0x4E01, new[]{"ding"}),        // 丁
            (0x4E03, 0x4E03, new[]{"qi"}),          // 七
            (0x4E07, 0x4E07, new[]{"wan"}),         // 万
            (0x4E08, 0x4E08, new[]{"zhang"}),       // 丈
            (0x4E09, 0x4E09, new[]{"san"}),         // 三
            (0x4E0A, 0x4E0A, new[]{"shang"}),       // 上
            (0x4E0B, 0x4E0B, new[]{"xia"}),         // 下
            (0x4E0D, 0x4E0D, new[]{"bu"}),          // 不
            (0x4E0E, 0x4E0E, new[]{"yu"}),          // 与
            (0x4E16, 0x4E16, new[]{"shi"}),         // 世
            (0x4E1A, 0x4E1A, new[]{"ye"}),          // 业
            (0x4E1C, 0x4E1C, new[]{"dong"}),        // 东
            (0x4E1D, 0x4E1D, new[]{"si"}),          // 丝
            (0x4E22, 0x4E22, new[]{"diu"}),         // 丢
            (0x4E24, 0x4E24, new[]{"liang"}),       // 两
            (0x4E25, 0x4E25, new[]{"yan"}),         // 严
            (0x4E2A, 0x4E2A, new[]{"ge"}),          // 个
            (0x4E2D, 0x4E2D, new[]{"zhong"}),       // 中
            (0x4E30, 0x4E30, new[]{"feng"}),        // 丰
            (0x4E34, 0x4E34, new[]{"lin"}),         // 临
            (0x4E3A, 0x4E3A, new[]{"wei"}),         // 为
            (0x4E3B, 0x4E3B, new[]{"zhu"}),         // 主
            (0x4E3D, 0x4E3D, new[]{"li"}),          // 丽
            (0x4E3E, 0x4E3E, new[]{"ju"}),          // 举
            (0x4E45, 0x4E45, new[]{"jiu"}),         // 久
            (0x4E48, 0x4E48, new[]{"me"}),          // 么
            (0x4E49, 0x4E49, new[]{"yi"}),          // 义
            (0x4E4B, 0x4E4B, new[]{"zhi"}),         // 之
            (0x4E4C, 0x4E4C, new[]{"wu"}),          // 乌
            (0x4E4E, 0x4E4E, new[]{"hu"}),          // 乎
            (0x4E50, 0x4E50, new[]{"le"}),          // 乐
            (0x4E56, 0x4E56, new[]{"guai"}),        // 乖
            (0x4E58, 0x4E58, new[]{"cheng"}),       // 乘
            (0x4E5D, 0x4E5D, new[]{"jiu"}),         // 九
            (0x4E5F, 0x4E5F, new[]{"ye"}),          // 也
            (0x4E60, 0x4E60, new[]{"xi"}),          // 习
            (0x4E61, 0x4E61, new[]{"xiang"}),       // 乡
            (0x4E66, 0x4E66, new[]{"shu"}),         // 书
            (0x4E70, 0x4E70, new[]{"mai"}),         // 买
            (0x4E71, 0x4E71, new[]{"luan"}),        // 乱
            (0x4E86, 0x4E86, new[]{"le"}),          // 了
            (0x4E8B, 0x4E8B, new[]{"shi"}),         // 事
            (0x4E8C, 0x4E8C, new[]{"er"}),          // 二
            (0x4E8E, 0x4E8E, new[]{"yu"}),          // 于
            (0x4E91, 0x4E91, new[]{"yun"}),         // 云
            (0x4E92, 0x4E92, new[]{"hu"}),          // 互
            (0x4E94, 0x4E94, new[]{"wu"}),          // 五
            (0x4E95, 0x4E95, new[]{"jing"}),        // 井
            (0x4E9A, 0x4E9A, new[]{"ya"}),          // 亚
            (0x4EA1, 0x4EA1, new[]{"wang"}),        // 亡
            (0x4EA4, 0x4EA4, new[]{"jiao"}),        // 交
            (0x4EA6, 0x4EA6, new[]{"yi"}),          // 亦
            (0x4EA7, 0x4EA7, new[]{"chan"}),         // 产
            (0x4EA9, 0x4EA9, new[]{"mu"}),          // 亩
            (0x4EAB, 0x4EAB, new[]{"xiang"}),       // 享
            (0x4EAC, 0x4EAC, new[]{"jing"}),        // 京
            (0x4EAE, 0x4EAE, new[]{"liang"}),       // 亮
            (0x4EB2, 0x4EB2, new[]{"qin"}),         // 亲
            (0x4EBA, 0x4EBA, new[]{"ren"}),         // 人
            (0x4EC0, 0x4EC0, new[]{"shen"}),        // 什
            (0x4EC1, 0x4EC1, new[]{"ren"}),         // 仁
            (0x4EC5, 0x4EC5, new[]{"jin"}),         // 仅
            (0x4EC7, 0x4EC7, new[]{"chou"}),        // 仇
            (0x4ECA, 0x4ECA, new[]{"jin"}),         // 今
            (0x4ECB, 0x4ECB, new[]{"jie"}),         // 介
            (0x4ECD, 0x4ECD, new[]{"reng"}),        // 仍
            (0x4ECE, 0x4ECE, new[]{"cong"}),        // 从
            (0x4ED4, 0x4ED4, new[]{"zi"}),          // 仔
            (0x4ED6, 0x4ED6, new[]{"ta"}),          // 他
            (0x4ED8, 0x4ED8, new[]{"fu"}),          // 付
            (0x4EE3, 0x4EE3, new[]{"dai"}),         // 代
            (0x4EE4, 0x4EE4, new[]{"ling"}),        // 令
            (0x4EE5, 0x4EE5, new[]{"yi"}),          // 以
            (0x4EEC, 0x4EEC, new[]{"men"}),         // 们
            (0x4EF6, 0x4EF6, new[]{"jian"}),        // 件
            (0x4EF7, 0x4EF7, new[]{"jia"}),         // 价
            (0x4EFB, 0x4EFB, new[]{"ren"}),         // 任
            (0x4EFD, 0x4EFD, new[]{"fen"}),         // 份
            (0x4F01, 0x4F01, new[]{"qi"}),          // 企
            (0x4F0A, 0x4F0A, new[]{"yi"}),          // 伊
            (0x4F0D, 0x4F0D, new[]{"wu"}),          // 伍
            (0x4F0F, 0x4F0F, new[]{"fu"}),          // 伏
            (0x4F11, 0x4F11, new[]{"xiu"}),         // 休
            (0x4F17, 0x4F17, new[]{"zhong"}),       // 众
            (0x4F18, 0x4F18, new[]{"you"}),         // 优
            (0x4F1A, 0x4F1A, new[]{"hui"}),         // 会
            (0x4F1F, 0x4F1F, new[]{"wei"}),         // 伟
            (0x4F20, 0x4F20, new[]{"chuan"}),       // 传
            (0x4F24, 0x4F24, new[]{"shang"}),       // 伤
            (0x4F26, 0x4F26, new[]{"lun"}),         // 伦
            (0x4F2F, 0x4F2F, new[]{"bo"}),          // 伯
            (0x4F30, 0x4F30, new[]{"gu"}),          // 估
            (0x4F34, 0x4F34, new[]{"ban"}),         // 伴
            (0x4F38, 0x4F38, new[]{"shen"}),        // 伸
            (0x4F3C, 0x4F3C, new[]{"si"}),          // 似
            (0x4F46, 0x4F46, new[]{"dan"}),         // 但
            (0x4F4D, 0x4F4D, new[]{"wei"}),         // 位
            (0x4F4E, 0x4F4E, new[]{"di"}),          // 低
            (0x4F4F, 0x4F4F, new[]{"zhu"}),         // 住
            (0x4F53, 0x4F53, new[]{"ti"}),          // 体
            (0x4F55, 0x4F55, new[]{"he"}),          // 何
            (0x4F59, 0x4F59, new[]{"yu"}),          // 余
            (0x4F5B, 0x4F5B, new[]{"fo"}),          // 佛
            (0x4F5C, 0x4F5C, new[]{"zuo"}),         // 作
            (0x4F60, 0x4F60, new[]{"ni"}),          // 你
            (0x4F73, 0x4F73, new[]{"jia"}),         // 佳
            (0x4F7F, 0x4F7F, new[]{"shi"}),         // 使
            (0x4F9B, 0x4F9B, new[]{"gong"}),        // 供
            (0x4F9D, 0x4F9D, new[]{"yi"}),          // 依
            (0x4FA0, 0x4FA0, new[]{"xia"}),         // 侠
            (0x4FA6, 0x4FA6, new[]{"zhen"}),        // 侦
            (0x4FA7, 0x4FA7, new[]{"ce"}),          // 侧
            (0x4FA8, 0x4FA8, new[]{"qiao"}),        // 侨
            (0x4FAF, 0x4FAF, new[]{"hou"}),         // 侯
            (0x4FB5, 0x4FB5, new[]{"qin"}),         // 侵
            (0x4FC3, 0x4FC3, new[]{"cu"}),          // 促
            (0x4FCA, 0x4FCA, new[]{"jun"}),         // 俊
            (0x4FD7, 0x4FD7, new[]{"su"}),          // 俗
            (0x4FDD, 0x4FDD, new[]{"bao"}),         // 保
            (0x4FE1, 0x4FE1, new[]{"xin"}),         // 信
            (0x4FEE, 0x4FEE, new[]{"xiu"}),         // 修
            (0x4FF1, 0x4FF1, new[]{"ju"}),          // 俱
            (0x500D, 0x500D, new[]{"bei"}),         // 倍
            (0x5012, 0x5012, new[]{"dao"}),         // 倒
            (0x5019, 0x5019, new[]{"hou"}),         // 候
            (0x501F, 0x501F, new[]{"jie"}),         // 借
            (0x5026, 0x5026, new[]{"juan"}),        // 倦
            (0x503C, 0x503C, new[]{"zhi"}),         // 值
            (0x5047, 0x5047, new[]{"jia"}),         // 假
            (0x504F, 0x504F, new[]{"pian"}),        // 偏
            (0x505A, 0x505A, new[]{"zuo"}),         // 做
            (0x505C, 0x505C, new[]{"ting"}),        // 停
            (0x5065, 0x5065, new[]{"jian"}),        // 健
            (0x50A8, 0x50A8, new[]{"chu"}),         // 储
            (0x50AC, 0x50AC, new[]{"cui"}),         // 催
            (0x50CF, 0x50CF, new[]{"xiang"}),       // 像
            (0x50F5, 0x50F5, new[]{"jiang"}),       // 僵
            (0x5112, 0x5112, new[]{"ru"}),          // 儒
            (0x513F, 0x513F, new[]{"er"}),          // 儿
            (0x5141, 0x5141, new[]{"yun"}),         // 允
            (0x5143, 0x5143, new[]{"yuan"}),        // 元
            (0x5144, 0x5144, new[]{"xiong"}),       // 兄
            (0x5145, 0x5145, new[]{"chong"}),       // 充
            (0x5148, 0x5148, new[]{"xian"}),        // 先
            (0x5149, 0x5149, new[]{"guang"}),       // 光
            (0x514B, 0x514B, new[]{"ke"}),          // 克
            (0x514D, 0x514D, new[]{"mian"}),        // 免
            (0x515A, 0x515A, new[]{"dang"}),        // 党
            (0x5165, 0x5165, new[]{"ru"}),          // 入
            (0x5168, 0x5168, new[]{"quan"}),        // 全
            (0x516B, 0x516B, new[]{"ba"}),          // 八
            (0x516C, 0x516C, new[]{"gong"}),        // 公
            (0x516D, 0x516D, new[]{"liu"}),         // 六
            (0x5170, 0x5170, new[]{"lan"}),         // 兰
            (0x5171, 0x5171, new[]{"gong"}),        // 共
            (0x5173, 0x5173, new[]{"guan"}),        // 关
            (0x5174, 0x5174, new[]{"xing"}),        // 兴
            (0x5175, 0x5175, new[]{"bing"}),        // 兵
            (0x5176, 0x5176, new[]{"qi"}),          // 其
            (0x5177, 0x5177, new[]{"ju"}),          // 具
            (0x5178, 0x5178, new[]{"dian"}),        // 典
            (0x517B, 0x517B, new[]{"yang"}),        // 养
            (0x517C, 0x517C, new[]{"jian"}),        // 兼
            (0x5185, 0x5185, new[]{"nei"}),         // 内
            (0x518C, 0x518C, new[]{"ce"}),          // 册
            (0x518D, 0x518D, new[]{"zai"}),         // 再
            (0x5192, 0x5192, new[]{"mao"}),         // 冒
            (0x5199, 0x5199, new[]{"xie"}),         // 写
            (0x519B, 0x519B, new[]{"jun"}),         // 军
            (0x519C, 0x519C, new[]{"nong"}),        // 农
            (0x51A0, 0x51A0, new[]{"guan"}),        // 冠
            (0x51AC, 0x51AC, new[]{"dong"}),        // 冬
            (0x51B0, 0x51B0, new[]{"bing"}),        // 冰
            (0x51B2, 0x51B2, new[]{"chong"}),       // 冲
            (0x51B3, 0x51B3, new[]{"jue"}),         // 决
            (0x51B5, 0x51B5, new[]{"kuang"}),       // 况
            (0x51B7, 0x51B7, new[]{"leng"}),        // 冷
            (0x51C6, 0x51C6, new[]{"zhun"}),        // 准
            (0x51CF, 0x51CF, new[]{"jian"}),        // 减
            (0x51E0, 0x51E0, new[]{"ji"}),          // 几
            (0x51E1, 0x51E1, new[]{"fan"}),         // 凡
            (0x51E4, 0x51E4, new[]{"feng"}),        // 凤
            (0x51ED, 0x51ED, new[]{"ping"}),        // 凭
            (0x51EF, 0x51EF, new[]{"kai"}),         // 凯
            (0x51F0, 0x51F0, new[]{"huang"}),       // 凰
            (0x51FA, 0x51FA, new[]{"chu"}),         // 出
            (0x51FB, 0x51FB, new[]{"ji"}),          // 击
            (0x51FD, 0x51FD, new[]{"han"}),         // 函
            (0x5200, 0x5200, new[]{"dao"}),         // 刀
            (0x5206, 0x5206, new[]{"fen"}),         // 分
            (0x5207, 0x5207, new[]{"qie"}),         // 切
            (0x520A, 0x520A, new[]{"kan"}),         // 刊
            (0x5211, 0x5211, new[]{"xing"}),        // 刑
            (0x5212, 0x5212, new[]{"hua"}),         // 划
            (0x5217, 0x5217, new[]{"lie"}),         // 列
            (0x5218, 0x5218, new[]{"liu"}),         // 刘
            (0x5219, 0x5219, new[]{"ze"}),          // 则
            (0x521A, 0x521A, new[]{"gang"}),        // 刚
            (0x521B, 0x521B, new[]{"chuang"}),      // 创
            (0x521D, 0x521D, new[]{"chu"}),         // 初
            (0x5224, 0x5224, new[]{"pan"}),         // 判
            (0x5229, 0x5229, new[]{"li"}),          // 利
            (0x522B, 0x522B, new[]{"bie"}),         // 别
            (0x5230, 0x5230, new[]{"dao"}),         // 到
            (0x5236, 0x5236, new[]{"zhi"}),         // 制
            (0x5237, 0x5237, new[]{"shua"}),        // 刷
            (0x523A, 0x523A, new[]{"ci"}),          // 刺
            (0x523B, 0x523B, new[]{"ke"}),          // 刻
            (0x524D, 0x524D, new[]{"qian"}),        // 前
            (0x5267, 0x5267, new[]{"ju"}),          // 剧
            (0x5269, 0x5269, new[]{"sheng"}),       // 剩
            (0x526A, 0x526A, new[]{"jian"}),        // 剪
            (0x526F, 0x526F, new[]{"fu"}),          // 副
            (0x5272, 0x5272, new[]{"ge"}),          // 割
            (0x529B, 0x529B, new[]{"li"}),          // 力
            (0x529E, 0x529E, new[]{"ban"}),         // 办
            (0x529F, 0x529F, new[]{"gong"}),        // 功
            (0x52A0, 0x52A0, new[]{"jia"}),         // 加
            (0x52A1, 0x52A1, new[]{"wu"}),          // 务
            (0x52A3, 0x52A3, new[]{"lie"}),         // 劣
            (0x52A8, 0x52A8, new[]{"dong"}),        // 动
            (0x52A9, 0x52A9, new[]{"zhu"}),         // 助
            (0x52AA, 0x52AA, new[]{"nu"}),          // 努
            (0x52B1, 0x52B1, new[]{"li"}),          // 励
            (0x52B2, 0x52B2, new[]{"jin"}),         // 劲
            (0x52BF, 0x52BF, new[]{"shi"}),         // 势
            (0x52C7, 0x52C7, new[]{"yong"}),        // 勇
            (0x52D2, 0x52D2, new[]{"le"}),          // 勒
            (0x52E4, 0x52E4, new[]{"qin"}),         // 勤
        };

        foreach (var (start, end, readings) in map)
        {
            for (int cp = start; cp <= end; cp++)
                Set((char)cp, readings[0]);
        }

        FillCommonCharacters();
    }

    private static void FillCommonCharacters()
    {
        var bulk = new Dictionary<string, string>
        {
            ["包北背本比笔毕必边变标表别病并补步部"] = "bao,bei,bei,ben,bi,bi,bi,bi,bian,bian,biao,biao,bie,bing,bing,bu,bu,bu",
            ["才材采彩菜参餐残藏操草策层曾察茶查差拆产昌尝常场厂唱超朝潮车彻沉陈称城成程承诚吃冲虫抽仇丑出初除础楚处触川穿船窗床创春纯词此次聪从粗促存错"] = "cai,cai,cai,cai,cai,can,can,can,cang,cao,cao,ce,ceng,ceng,cha,cha,cha,cha,chai,chan,chang,chang,chang,chang,chang,chang,chao,chao,chao,che,che,chen,chen,cheng,cheng,cheng,cheng,cheng,cheng,chi,chong,chong,chou,chou,chou,chu,chu,chu,chu,chu,chu,chu,chuan,chuan,chuan,chuang,chuang,chuang,chun,chun,ci,ci,ci,cong,cong,cu,cu,cun,cuo",
            ["达答打带待袋戴丹单胆旦但蛋弹岛导倒盗到道德灯登等低底抵敌的滴敌地弟递第点典电店淀调掉丁顶定丢冬东董懂洞动冻都斗豆毒独读度渡短断段锻堆队对吨蹲顿多夺朵"] = "da,da,da,dai,dai,dai,dai,dan,dan,dan,dan,dan,dan,dan,dao,dao,dao,dao,dao,dao,de,deng,deng,deng,di,di,di,di,de,di,di,di,di,di,di,dian,dian,dian,dian,dian,diao,diao,ding,ding,ding,diu,dong,dong,dong,dong,dong,dong,dong,dou,dou,dou,du,du,du,du,du,duan,duan,duan,duan,dui,dui,dui,dun,dun,dun,duo,duo,duo",
            ["额恩耳二"] = "e,en,er,er",
            ["法帆翻凡烦反返范犯饭方房防妨仿访纺放飞非肥废费分纷芬坟粉份丰风封疯锋逢缝凤否夫扶服浮福符幅辐抚府腐父付妇附复赋富腹覆"] = "fa,fan,fan,fan,fan,fan,fan,fan,fan,fan,fang,fang,fang,fang,fang,fang,fang,fang,fei,fei,fei,fei,fei,fen,fen,fen,fen,fen,fen,feng,feng,feng,feng,feng,feng,feng,feng,fou,fu,fu,fu,fu,fu,fu,fu,fu,fu,fu,fu,fu,fu,fu,fu,fu,fu,fu,fu,fu",
            ["该改盖概感敢赶干刚钢高搞稿告哥歌革格隔个各给根跟更工公功攻供宫恭巩共勾沟狗构购够估古骨谷股故顾固瓜挂拐怪关观官管馆惯灌光广归龟规轨鬼贵桂滚锅国果过"] = "gai,gai,gai,gai,gan,gan,gan,gan,gang,gang,gao,gao,gao,gao,ge,ge,ge,ge,ge,ge,ge,gei,gen,gen,geng,gong,gong,gong,gong,gong,gong,gong,gong,gong,gou,gou,gou,gou,gou,gou,gu,gu,gu,gu,gu,gu,gu,gu,gua,gua,guai,guai,guan,guan,guan,guan,guan,guan,guan,guang,guang,gui,gui,gui,gui,gui,gui,gui,gun,guo,guo,guo,guo",
            ["哈海害含寒喊汉汗旱航毫豪好号耗喝河合何核荷盒贺黑恨很横衡红宏洪虹厚后猴吼呼忽狐胡湖壶虎互户护花华滑化划画话怀坏欢环还换唤患荒慌皇黄灰恢挥辉回汇毁悔会绘惠慧婚混活火伙或货获"] = "ha,hai,hai,han,han,han,han,han,han,hang,hao,hao,hao,hao,hao,he,he,he,he,he,he,he,he,hei,hen,hen,heng,heng,hong,hong,hong,hong,hou,hou,hou,hou,hu,hu,hu,hu,hu,hu,hu,hu,hu,hu,hua,hua,hua,hua,hua,hua,hua,huai,huai,huan,huan,hai,huan,huan,huan,huang,huang,huang,huang,hui,hui,hui,hui,hui,hui,hui,hui,hui,hui,hui,hui,hun,hun,huo,huo,huo,huo,huo,huo",
            ["机鸡积基激及吉极即级急疾集籍几己济记纪既技际季计继寂绩加家佳夹嘉甲假价架驾嫁歼坚尖检简减剪建件健渐践鉴键箭江姜将浆奖讲降酱交郊骄胶焦角脚教较接揭街阶节结劫杰洁姐解介戒届界今金斤仅紧锦尽近进晋禁京经惊精井景警净竞境静镜敬究九酒久救就旧居局菊橘举巨句拒具距聚卷决绝觉军均君"] = "ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,ji,jia,jia,jia,jia,jia,jia,jia,jia,jia,jia,jia,jian,jian,jian,jian,jian,jian,jian,jian,jian,jian,jian,jian,jian,jian,jian,jiang,jiang,jiang,jiang,jiang,jiang,jiang,jiang,jiao,jiao,jiao,jiao,jiao,jiao,jiao,jiao,jiao,jie,jie,jie,jie,jie,jie,jie,jie,jie,jie,jie,jie,jie,jie,jie,jin,jin,jin,jin,jin,jin,jin,jin,jin,jin,jin,jing,jing,jing,jing,jing,jing,jing,jing,jing,jing,jing,jing,jing,jiu,jiu,jiu,jiu,jiu,jiu,jiu,ju,ju,ju,ju,ju,ju,ju,ju,ju,ju,ju,juan,jue,jue,jue,jun,jun,jun",
            ["开揭凯刊勘看康抗考烤靠科棵颗壳可渴克刻客课肯坑空孔恐控口扣枯哭苦库裤酷快宽款狂况矿亏葵昆困扩括"] = "kai,jie,kai,kan,kan,kan,kang,kang,kao,kao,kao,ke,ke,ke,ke,ke,ke,ke,ke,ke,ke,ken,keng,kong,kong,kong,kong,kou,kou,ku,ku,ku,ku,ku,ku,kuai,kuan,kuan,kuang,kuang,kuang,kui,kui,kun,kun,kuo,kuo",
            ["拉啦来赖兰蓝篮栏拦懒烂滥郎朗浪劳牢老姥雷累泪类冷离梨黎礼李里理力历立丽利励例隶粒俩连联莲怜帘脸练炼链良凉梁粮两辆亮谅疗辽料列烈裂猎林临邻领灵铃凌零另令留流柳六龙聋隆楼漏露卢芦鲁陆录路绿驴律虑率略轮论罗萝逻落骆"] = "la,la,lai,lai,lan,lan,lan,lan,lan,lan,lan,lan,lang,lang,lang,lao,lao,lao,lao,lei,lei,lei,lei,leng,li,li,li,li,li,li,li,li,li,li,li,li,li,li,li,li,lia,lian,lian,lian,lian,lian,lian,lian,lian,lian,liang,liang,liang,liang,liang,liang,liang,liang,liao,liao,liao,lie,lie,lie,lie,lin,lin,lin,ling,ling,ling,ling,ling,ling,ling,liu,liu,liu,liu,long,long,long,lou,lou,lu,lu,lu,lu,lu,lu,lu,lv,lv,lv,lv,lv,lve,lun,lun,luo,luo,luo,luo,luo",
            ["妈麻马骂吗嘛买卖麦迈满蛮瞒漫忙猫毛矛茅冒帽贸每美妹门萌盟蒙梦迷谜弥密蜜眠免面苗描秒庙灭民敏名明命模膜摸磨末莫墨默谋某木目牧墓幕慕暮母"] = "ma,ma,ma,ma,ma,ma,mai,mai,mai,mai,man,man,man,man,mang,mao,mao,mao,mao,mao,mao,mao,mei,mei,mei,men,meng,meng,meng,meng,mi,mi,mi,mi,mi,mian,mian,mian,miao,miao,miao,miao,mie,min,min,ming,ming,ming,mo,mo,mo,mo,mo,mo,mo,mo,mou,mou,mu,mu,mu,mu,mu,mu,mu,mu",
            ["拿哪那纳乃奶耐南男难脑恼闹呢嫩能泥尼拟你年念娘鸟尿捏您宁牛扭农浓弄奴怒女暖虐挪诺"] = "na,na,na,na,nai,nai,nai,nan,nan,nan,nao,nao,nao,ne,nen,neng,ni,ni,ni,ni,nian,nian,niang,niao,niao,nie,nin,ning,niu,niu,nong,nong,nong,nu,nu,nv,nuan,nve,nuo,nuo",
            ["哦偶"] = "o,ou",
            ["怕拍排牌派攀盘判盼旁庞胖抛跑泡炮陪培赔佩配盆喷朋棚捧碰批皮匹篇偏片骗漂飘票拼品贫频评凭苹屏瓶平迫破魄铺扑葡朴普谱"] = "pa,pai,pai,pai,pai,pan,pan,pan,pan,pang,pang,pang,pao,pao,pao,pao,pei,pei,pei,pei,pei,pen,pen,peng,peng,peng,peng,pi,pi,pi,pian,pian,pian,pian,piao,piao,piao,pin,pin,pin,pin,ping,ping,ping,ping,ping,ping,po,po,po,pu,pu,pu,pu,pu,pu",
            ["七妻期欺齐其棋奇骑旗企启起气弃器千迁签铅前钱浅遣歉枪腔强墙抢悄巧桥乔侨瞧切茄且窃亲琴勤青轻氢倾清情晴请庆穷丘秋球求区曲取趣去圈权全泉拳犬劝缺却确裙群"] = "qi,qi,qi,qi,qi,qi,qi,qi,qi,qi,qi,qi,qi,qi,qi,qi,qian,qian,qian,qian,qian,qian,qian,qian,qian,qiang,qiang,qiang,qiang,qiang,qiao,qiao,qiao,qiao,qiao,qiao,qie,qie,qie,qie,qin,qin,qin,qing,qing,qing,qing,qing,qing,qing,qing,qing,qiong,qiu,qiu,qiu,qiu,qu,qu,qu,qu,qu,quan,quan,quan,quan,quan,quan,quan,que,que,que,qun,qun",
            ["然燃染嚷让饶扰绕热壬人仁忍认任扔日容绒荣融柔肉如乳入软锐瑞润弱"] = "ran,ran,ran,rang,rang,rao,rao,rao,re,ren,ren,ren,ren,ren,ren,reng,ri,rong,rong,rong,rong,rou,rou,ru,ru,ru,ruan,rui,rui,run,ruo",
            ["洒撒塞赛三伞散桑嗓丧扫嫂色森杀沙纱傻啥晒山删闪善伤商赏上尚烧稍少绍奢舌蛇设射摄社舍申伸身深神审慎升生声胜绳省圣盛剩尸失师诗施湿十什石识时实拾食史始驶士示世市事势视试饰室是释收手守首受寿授售书叔殊舒输蔬熟鼠属术束述树竖数刷耍衰摔甩帅双霜爽谁水睡顺说丝司私思斯死四似寺松宋送诵搜艘苏俗素速宿塑酸蒜算虽随岁碎孙损缩所索锁"] = "sa,sa,sai,sai,san,san,san,sang,sang,sang,sao,sao,se,sen,sha,sha,sha,sha,sha,shai,shan,shan,shan,shan,shang,shang,shang,shang,shang,shao,shao,shao,shao,she,she,she,she,she,she,she,she,shen,shen,shen,shen,shen,shen,shen,sheng,sheng,sheng,sheng,sheng,sheng,sheng,sheng,sheng,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shi,shou,shou,shou,shou,shou,shou,shou,shou,shu,shu,shu,shu,shu,shu,shu,shu,shu,shu,shu,shu,shu,shu,shu,shua,shua,shuai,shuai,shuai,shuai,shuang,shuang,shuang,shui,shui,shui,shun,shuo,si,si,si,si,si,si,si,si,si,song,song,song,song,sou,sou,su,su,su,su,su,su,suan,suan,suan,sui,sui,sui,sui,sun,sun,suo,suo,suo,suo",
            ["他她它踏台太态抬摊滩坛谈弹毯探叹汤唐堂塘糖躺趟掏逃桃陶淘讨套特疼腾梯踢提题蹄体替天田甜添填挑条跳贴铁厅听庭停亭挺通同铜童统痛偷头投透突图徒途涂土吐兔团推退吞屯托脱拖"] = "ta,ta,ta,ta,tai,tai,tai,tai,tan,tan,tan,tan,dan,tan,tan,tan,tang,tang,tang,tang,tang,tang,tang,tao,tao,tao,tao,tao,tao,tao,te,teng,teng,ti,ti,ti,ti,ti,ti,ti,tian,tian,tian,tian,tian,tiao,tiao,tiao,tie,tie,ting,ting,ting,ting,ting,ting,tong,tong,tong,tong,tong,tong,tou,tou,tou,tou,tu,tu,tu,tu,tu,tu,tu,tu,tuan,tui,tui,tun,tun,tuo,tuo,tuo",
            ["挖哇娃瓦歪外弯湾丸完玩晚碗万汪王网往忘旺望危威微巍违围唯维伟尾纬未位味畏胃卫温文闻蚊稳问翁窝我沃握乌污屋无吴五午武舞务物误雾"] = "wa,wa,wa,wa,wai,wai,wan,wan,wan,wan,wan,wan,wan,wan,wang,wang,wang,wang,wang,wang,wang,wei,wei,wei,wei,wei,wei,wei,wei,wei,wei,wei,wei,wei,wei,wei,wei,wei,wen,wen,wen,wen,wen,wen,weng,wo,wo,wo,wo,wu,wu,wu,wu,wu,wu,wu,wu,wu,wu,wu,wu,wu",
            ["西吸希析息牺悉惜稀溪锡熙膝习席袭洗喜戏系细瞎虾峡狭下吓夏仙先鲜纤咸贤弦闲显险县现线宪陷献乡相香箱湘想响享项象像橡消削小晓孝笑效校些歇协写血泄卸屑谢心辛新信星兴形型醒杏姓幸性凶兄胸雄休修秀袖绣虚需许叙序续蓄宣悬选旋穴学雪血勋寻巡训讯迅压呀牙芽崖丫鸦雅"] = "xi,xi,xi,xi,xi,xi,xi,xi,xi,xi,xi,xi,xi,xi,xi,xi,xi,xi,xi,xi,xi,xia,xia,xia,xia,xia,xia,xia,xian,xian,xian,xian,xian,xian,xian,xian,xian,xian,xian,xian,xian,xian,xian,xian,xiang,xiang,xiang,xiang,xiang,xiang,xiang,xiang,xiang,xiang,xiang,xiang,xiao,xiao,xiao,xiao,xiao,xiao,xiao,xiao,xie,xie,xie,xie,xie,xie,xie,xie,xie,xin,xin,xin,xin,xing,xing,xing,xing,xing,xing,xing,xing,xing,xiong,xiong,xiong,xiong,xiu,xiu,xiu,xiu,xiu,xu,xu,xu,xu,xu,xu,xu,xuan,xuan,xuan,xuan,xue,xue,xue,xue,xun,xun,xun,xun,xun,xun,ya,ya,ya,ya,ya,ya,ya,ya",
            ["烟淹盐严颜阎延言岩沿眼演验焰宴燕央扬阳杨洋仰养样腰遥摇药要邀耀爷也野业叶页夜液一衣医依壹宜移仪遗疑已以蚁义亿忆艺议译易益谊因阴音银引饮印应英婴樱营影迎映硬赢用尤由油游犹有友右又幼于与宇语羽雨玉预域欲育遇愈元园原员圆援源远院愿约月越跃阅云允运晕韵孕"] = "yan,yan,yan,yan,yan,yan,yan,yan,yan,yan,yan,yan,yan,yan,yan,yan,yang,yang,yang,yang,yang,yang,yang,yang,yao,yao,yao,yao,yao,yao,yao,ye,ye,ye,ye,ye,ye,ye,ye,yi,yi,yi,yi,yi,yi,yi,yi,yi,yi,yi,yi,yi,yi,yi,yi,yi,yi,yi,yi,yi,yi,yin,yin,yin,yin,yin,yin,yin,ying,ying,ying,ying,ying,ying,ying,ying,ying,ying,yong,you,you,you,you,you,you,you,you,you,you,yu,yu,yu,yu,yu,yu,yu,yu,yu,yu,yu,yu,yu,yuan,yuan,yuan,yuan,yuan,yuan,yuan,yuan,yuan,yuan,yue,yue,yue,yue,yue,yun,yun,yun,yun,yun,yun",
            ["杂砸灾栽载再在赞暂脏藏葬遭糟早澡造则责择泽贼怎增赠扎眨炸摘宅窄寨占战站张章彰障招找召兆赵照罩遮折哲者这针侦珍真阵振震镇争征整正证政郑挣睁蒸织职直值植殖指止址纸志至致制质治秩智置中终忠钟种肿众周洲舟粥州轴宙昼皱骤猪珠竹蛛主柱注住驻助祝著筑抓爪专砖转赚庄装壮撞追准捉桌着资姿紫字自宗综踪总纵走奏租族足阻组祖嘴最罪醉尊遵昨左坐座做"] = "za,za,zai,zai,zai,zai,zai,zan,zan,zang,zang,zang,zao,zao,zao,zao,zao,ze,ze,ze,ze,zei,zen,zeng,zeng,zha,zha,zha,zhai,zhai,zhai,zhai,zhan,zhan,zhan,zhang,zhang,zhang,zhang,zhao,zhao,zhao,zhao,zhao,zhao,zhao,zhe,zhe,zhe,zhe,zhe,zhen,zhen,zhen,zhen,zhen,zhen,zhen,zhen,zheng,zheng,zheng,zheng,zheng,zheng,zheng,zheng,zheng,zheng,zhi,zhi,zhi,zhi,zhi,zhi,zhi,zhi,zhi,zhi,zhi,zhi,zhi,zhi,zhi,zhi,zhi,zhi,zhi,zhong,zhong,zhong,zhong,zhong,zhong,zhong,zhou,zhou,zhou,zhou,zhou,zhou,zhou,zhou,zhou,zhou,zhu,zhu,zhu,zhu,zhu,zhu,zhu,zhu,zhu,zhu,zhu,zhu,zhu,zhua,zhua,zhuan,zhuan,zhuan,zhuan,zhuang,zhuang,zhuang,zhuang,zhui,zhun,zhuo,zhuo,zhuo,zi,zi,zi,zi,zi,zong,zong,zong,zong,zong,zou,zou,zu,zu,zu,zu,zu,zu,zui,zui,zui,zui,zun,zun,zuo,zuo,zuo,zuo,zuo",
        };

        foreach (var (chars, pyList) in bulk)
        {
            var pys = pyList.Split(',');
            for (int i = 0; i < chars.Length && i < pys.Length; i++)
            {
                var ch = chars[i];
                if (ch >= '\u4E00' && ch <= '\u9FFF' && SingleReadings[ch - '\u4E00'] == null)
                    Set(ch, pys[i].Trim());
            }
        }
    }
}

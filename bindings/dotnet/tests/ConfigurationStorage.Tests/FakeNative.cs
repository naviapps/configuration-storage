using System.Collections.Concurrent;
using System.Runtime.InteropServices;
using System.Text;
using NaviApps.ConfigurationStorage;

sealed class FakeNative : IStorageNative
{
    private readonly ConcurrentDictionary<IntPtr, ConfigurationStorageResultView> results = new();
    private long sequence = 1;
    internal static IEnumerable<uint> IncompatibleVersions =>
        Enumerable.Range(0, (int)PInvokeStorageNative.SupportedAbiVersion).Select(value => (uint)value)
            .Append(PInvokeStorageNative.SupportedAbiVersion + 1);
    internal uint Version = PInvokeStorageNative.SupportedAbiVersion;
    internal string? OpenFailure;
    internal bool NullOpen;
    internal bool NullDispatch;
    internal uint ReplyCode;
    internal string Reply = Initialized;
    internal Action? OnOpen;
    internal Action? OnDispatch;
    internal Action? OnAbi;
    internal Action? OnRecoveryCopyDispatch;
    internal Func<ConfigurationStorageResultView, ConfigurationStorageResultView>? RewriteView;
    internal int Opens;
    internal int ExistingOpens;
    internal int AbiCalls;
    internal int Dispatches;
    internal int RecoveryCopyDispatches;
    internal int StoreDestroys;
    internal int ResultDestroys;
    internal int Active;
    internal int MaximumActive;
    internal string? ConfigurationJson;
    internal string? CommandJson;
    internal string? RecoveryCopyCommandJson;
    internal const string Generation = "opaque-content-bound-generation";
    internal const string Initialized = "{\"generation\":\"" + Generation + "\",\"payload\":\"AQID\"}";

    private IntPtr Result(uint code, string json)
    {
        var bytes = Encoding.UTF8.GetBytes(json);
        var pointer = Marshal.AllocHGlobal(bytes.Length);
        Marshal.Copy(bytes, 0, pointer, bytes.Length);
        var result = checked((IntPtr)Interlocked.Increment(ref sequence));
        results[result] = new(code, pointer, (nuint)bytes.Length);
        return result;
    }
    public uint AbiVersion()
    {
        Interlocked.Increment(ref AbiCalls);
        OnAbi?.Invoke();
        return Version;
    }
    public IntPtr Open(byte[] configuration, out IntPtr error)
    {
        Interlocked.Increment(ref Opens);
        return OpenCore(configuration, out error);
    }
    public IntPtr OpenExisting(byte[] configuration, out IntPtr error)
    {
        Interlocked.Increment(ref ExistingOpens);
        return OpenCore(configuration, out error);
    }
    private IntPtr OpenCore(byte[] configuration, out IntPtr error)
    {
        if (!Thread.CurrentThread.IsThreadPoolThread || AbiCalls == 0 || Version != PInvokeStorageNative.SupportedAbiVersion)
            throw new System.Diagnostics.UnreachableException("Open bypassed its executor or ABI validation.");
        ConfigurationJson = Encoding.UTF8.GetString(configuration);
        OnOpen?.Invoke();
        error = OpenFailure is { } failure ? Result(1, failure) : IntPtr.Zero;
        return error != IntPtr.Zero || NullOpen ? IntPtr.Zero : (IntPtr)1;
    }
    public IntPtr Dispatch(IntPtr store, byte[] command)
    {
        if (!Thread.CurrentThread.IsThreadPoolThread || store != (IntPtr)1 || StoreDestroys != 0)
            throw new System.Diagnostics.UnreachableException("Invalid executor or native owner lifetime.");
        Interlocked.Increment(ref Dispatches);
        int active = Interlocked.Increment(ref Active);
        MaximumActive = Math.Max(MaximumActive, active);
        try
        {
            CommandJson = Encoding.UTF8.GetString(command);
            OnDispatch?.Invoke();
            return NullDispatch ? IntPtr.Zero : Result(ReplyCode, Reply);
        }
        finally { Interlocked.Decrement(ref Active); }
    }
    public IntPtr RecoveryCopyDispatch(byte[] command)
    {
        if (!Thread.CurrentThread.IsThreadPoolThread || AbiCalls == 0 || Version != PInvokeStorageNative.SupportedAbiVersion)
            throw new System.Diagnostics.UnreachableException("Recovery dispatch bypassed its executor or ABI validation.");
        Interlocked.Increment(ref RecoveryCopyDispatches);
        RecoveryCopyCommandJson = Encoding.UTF8.GetString(command);
        OnRecoveryCopyDispatch?.Invoke();
        return NullDispatch ? IntPtr.Zero : Result(ReplyCode, Reply);
    }
    public void DestroyStore(IntPtr store)
    {
        if (store != (IntPtr)1 || Active != 0 || Interlocked.Increment(ref StoreDestroys) != 1)
            throw new System.Diagnostics.UnreachableException("Store destroyed twice or while dispatch is running.");
    }
    public ConfigurationStorageResultView View(IntPtr result) => RewriteView is { } rewrite ? rewrite(results[result]) : results[result];
    public void DestroyResult(IntPtr result)
    {
        if (!results.TryRemove(result, out var view)) throw new System.Diagnostics.UnreachableException("Result released twice.");
        Marshal.FreeHGlobal(view.Data);
        Interlocked.Increment(ref ResultDestroys);
    }
}

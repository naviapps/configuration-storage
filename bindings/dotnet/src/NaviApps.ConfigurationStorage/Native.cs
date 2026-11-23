using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

[assembly: InternalsVisibleTo("ConfigurationStorage.Tests")]

namespace NaviApps.ConfigurationStorage;

[StructLayout(LayoutKind.Sequential)]
internal readonly struct ConfigurationStorageResultView(
    uint code, IntPtr data, nuint length, IntPtr payload = default, nuint payloadLength = 0)
{
    public readonly uint Code = code;
    public readonly IntPtr Data = data;
    public readonly nuint Length = length;
    public readonly IntPtr Payload = payload;
    public readonly nuint PayloadLength = payloadLength;
}

internal interface IStorageNative
{
    uint AbiVersion();
    IntPtr Open(byte[] configuration, out IntPtr error);
    IntPtr OpenExisting(byte[] configuration, out IntPtr error);
    IntPtr Dispatch(IntPtr store, byte[] command, byte[]? payload);
    IntPtr RecoveryCopyDispatch(byte[] command, byte[]? payload);
    void DestroyStore(IntPtr store);
    ConfigurationStorageResultView View(IntPtr result);
    void DestroyResult(IntPtr result);
}

internal sealed class StoreHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    private readonly IStorageNative native;
    internal StoreHandle(IntPtr pointer, IStorageNative native) : base(true)
    {
        this.native = native;
        SetHandle(pointer);
    }
    protected override bool ReleaseHandle() { native.DestroyStore(handle); return true; }
}

internal sealed class ResultHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    private readonly IStorageNative native;
    internal ResultHandle(IntPtr pointer, IStorageNative native) : base(true)
    {
        this.native = native;
        SetHandle(pointer);
    }
    protected override bool ReleaseHandle() { native.DestroyResult(handle); return true; }
}

internal sealed partial class PInvokeStorageNative : IStorageNative
{
    internal const uint SupportedAbiVersion = 14;
    private const string Library = "configuration_storage";
    internal static readonly PInvokeStorageNative Instance = new();
    private PInvokeStorageNative() { }

    internal static void ValidateAbi(IStorageNative native)
    {
        if (native.AbiVersion() != SupportedAbiVersion)
            throw new StorageException(new(ErrorCode.UnsupportedAbiVersion));
    }

    [DllImport(Library, EntryPoint = "configuration_storage_abi_version", CallingConvention = CallingConvention.Cdecl)]
    private static extern uint Abi();
    [DllImport(Library, EntryPoint = "configuration_storage_store_open", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr StoreOpen(byte[] data, nuint length, out IntPtr error);
    [DllImport(Library, EntryPoint = "configuration_storage_store_open_existing", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr StoreOpenExisting(byte[] data, nuint length, out IntPtr error);
    [DllImport(Library, EntryPoint = "configuration_storage_store_request", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr StoreDispatch(IntPtr store, byte[] data, nuint length, byte[]? payload, nuint payloadLength);
    [DllImport(Library, EntryPoint = "configuration_storage_recovery_copy_request", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr DispatchRecoveryCopy(byte[] data, nuint length, byte[]? payload, nuint payloadLength);
    [DllImport(Library, EntryPoint = "configuration_storage_store_destroy", CallingConvention = CallingConvention.Cdecl)]
    private static extern void StoreDestroy(IntPtr store);
    [DllImport(Library, EntryPoint = "configuration_storage_result_view_v14", CallingConvention = CallingConvention.Cdecl)]
    private static extern ConfigurationStorageResultView ViewResult(IntPtr result);
    [DllImport(Library, EntryPoint = "configuration_storage_result_destroy", CallingConvention = CallingConvention.Cdecl)]
    private static extern void ResultDestroy(IntPtr result);

    public uint AbiVersion() => Abi();
    public IntPtr Open(byte[] configuration, out IntPtr error) => StoreOpen(configuration, (nuint)configuration.Length, out error);
    public IntPtr OpenExisting(byte[] configuration, out IntPtr error) => StoreOpenExisting(configuration, (nuint)configuration.Length, out error);
    public IntPtr Dispatch(IntPtr store, byte[] command, byte[]? payload)
        => StoreDispatch(store, command, (nuint)command.Length, payload is { Length: > 0 } ? payload : null, (nuint)(payload?.Length ?? 0));
    public IntPtr RecoveryCopyDispatch(byte[] command, byte[]? payload)
        => DispatchRecoveryCopy(command, (nuint)command.Length, payload is { Length: > 0 } ? payload : null, (nuint)(payload?.Length ?? 0));
    public void DestroyStore(IntPtr store) => StoreDestroy(store);
    public ConfigurationStorageResultView View(IntPtr result) => ViewResult(result);
    public void DestroyResult(IntPtr result) => ResultDestroy(result);
}

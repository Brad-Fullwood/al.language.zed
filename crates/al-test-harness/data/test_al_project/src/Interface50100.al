interface "ITest Processor"
{
    procedure Process(InputValue: Text): Boolean;
    procedure GetLastResult(): Text;
    procedure Reset();
}

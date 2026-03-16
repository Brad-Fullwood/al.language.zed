codeunit 50104 "Multi Procedure"
{
    var
        GlobalCounter: Integer;
        GlobalName: Text[100];

    procedure SimpleProc()
    begin
        GlobalCounter += 1;
    end;

    procedure WithReturn(): Boolean
    begin
        exit(GlobalCounter > 0);
    end;

    procedure WithParams(a: Integer; b: Text)
    var
        LocalVar: Integer;
    begin
        LocalVar := a * 2;
        GlobalName := b;
    end;

    local procedure LocalHelper(): Text
    begin
        exit(GlobalName);
    end;

    procedure VarParams(var Counter: Integer; Name: Text)
    begin
        Counter += 1;
        GlobalName := Name;
    end;

    procedure MultiReturn(Input: Integer): Integer
    var
        Temp: Integer;
    begin
        Temp := Input * GlobalCounter;
        if Temp > 100 then
            exit(100);
        exit(Temp);
    end;

    procedure WithRecord(var Rec: Record "Test Customer")
    begin
        Rec.Name := 'Updated';
    end;

    procedure CallsOthers()
    var
        Result: Boolean;
        Value: Integer;
    begin
        SimpleProc();
        Result := WithReturn();
        WithParams(42, 'test');
        Value := MultiReturn(10);
    end;

    procedure WithEnum(Status: Enum "Test Status"): Boolean
    begin
        exit(Status.AsInteger() > 0);
    end;

    procedure LongSignature(
        Param1: Integer;
        Param2: Text;
        Param3: Boolean;
        Param4: Decimal;
        Param5: Date;
        Param6: Code[20];
        Param7: Guid;
        Param8: DateTime
    ): Text
    begin
        exit('done');
    end;
}

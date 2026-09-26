interface "Fixture Shipper"
{
    procedure Ship(Qty: Integer);
}

codeunit 50300 "Fixture Truck" implements "Fixture Shipper"
{
    procedure Ship(Qty: Integer)
    var
        Entry: Record "Fixture Entry";
    begin
        Entry.Insert(true);
        OnAfterShip(Qty);
    end;

    [IntegrationEvent(false, false)]
    local procedure OnAfterShip(Qty: Integer)
    begin
    end;
}

codeunit 50301 "Fixture Dispatcher"
{
    trigger OnRun()
    begin
        TryPost();
    end;

    procedure Dispatch(Shipper: Interface "Fixture Shipper")
    var
        Truck: Codeunit "Fixture Truck";
    begin
        Shipper.Ship(1);
        Truck.Ship(2);
        Codeunit.Run(Codeunit::"Fixture Truck");
        Helper();
        Helper(1);
    end;

    local procedure Helper()
    begin
        Commit();
    end;

    local procedure Helper(Value: Integer)
    var
        Entry: Record "Fixture Entry";
    begin
        Entry.Modify();
    end;

    [TryFunction]
    procedure TryPost()
    var
        Entry: Record "Fixture Entry";
        Buffer: Record "Fixture Entry" temporary;
    begin
        Entry.Delete();
        Buffer.Insert();
    end;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Fixture Truck", 'OnAfterShip', '', false, false)]
    local procedure HandleShip(Qty: Integer)
    begin
        Dispatch(Qty);
    end;

    [EventSubscriber(ObjectType::Table, Database::"Fixture Entry", 'OnAfterInsertEvent', '', false, false)]
    local procedure HandleEntryInsert(var Rec: Record "Fixture Entry"; RunTrigger: Boolean)
    begin
        Helper();
    end;

    procedure Tidy(var Target: RecordRef)
    var
        Entry: Record "Fixture Entry";
        Card: Page "Fixture Card";
    begin
        Entry.Validate("No.", 'A');
        Entry.ModifyAll("No.", 'B', true);
        Entry.DeleteAll();
        Target.Modify();
        Card.RunModal();
    end;
}

table 50302 "Fixture Entry"
{
    fields
    {
        field(1; "No."; Code[20]) { }
    }

    trigger OnInsert()
    begin
    end;
}
